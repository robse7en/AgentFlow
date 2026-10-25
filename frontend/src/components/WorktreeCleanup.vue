<script lang="ts" setup>
import { computed, onMounted, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { useTaskStore } from '@/stores/tasks';
import { useOperationStore } from '@/stores/operations';
import { api, ApiError } from '@/api/client';
import type { WorktreeCleanupPreview } from '@/api/types';

/** The backend refused because the branch isn't the reviewed checkpoint or in a merged PR. */
const BRANCH_UNVERIFIED_CODE = 'branch_unverified';

const props = defineProps<{
    taskKey: string;
    expectedSha: string | null;
    worktreeExists: boolean | null;
    workerWorktreeCount: number | null;
    /** Skip the trigger button and go straight to the confirmation (e.g. inside a modal). */
    startOpen?: boolean;
}>();
const emit = defineEmits<{ close: [] }>();
const store = useTaskStore();
const operations = useOperationStore();
const route = useRoute();
const router = useRouter();
/** Cleanup (or a delete) already running for this task: its actions wait on the same lock. */
const running = computed(() => operations.runningFor(props.taskKey));
// Fail open: only treat it as already-clean when we positively know there's nothing
// left (worktreeExists is explicitly false and there are no worker worktrees either).
const alreadyClean = computed(() => props.worktreeExists === false && (props.workerWorktreeCount ?? 0) === 0);
const confirming = ref(false);
const busy = ref(false);
const preview = ref<WorktreeCleanupPreview | null>(null);
const previewError = ref('');
// Sticky once refused for an unverified branch, so a failed forced attempt keeps the option.
const branchUnverified = ref(false);

const MAX_LISTED = 8;

const openConfirm = async (unverified = false): Promise<void> => {
    confirming.value = true;
    previewError.value = '';
    preview.value = null;
    branchUnverified.value = unverified;
    busy.value = true;
    try {
        preview.value = await api.worktreeCleanupPreview(props.taskKey);
    } catch (cause) {
        previewError.value = cause instanceof Error ? cause.message : 'Could not check the worktree.';
    } finally {
        busy.value = false;
    }
};

const keep = (): void => {
    confirming.value = false;
    if (props.startOpen) emit('close');
};

onMounted(() => { if (props.startOpen) void openConfirm(); });

// A background cleanup refused for an unverified branch sends the user here (?cleanup=force) to decide on forcing it.
watch(() => route.query.cleanup, (value) => {
    if (value !== 'force' || props.startOpen) return;
    void router.replace({ query: { ...route.query, cleanup: undefined } });
    void openConfirm(true);
}, { immediate: true });

/** Removal can take minutes (ignored files, Windows retries): it runs on and a toast reports the outcome. */
const cleanup = (force = false): void => {
    const sha = props.expectedSha;
    if (!sha || busy.value || running.value) return;
    const key = props.taskKey;
    confirming.value = false;
    branchUnverified.value = false;
    if (props.startOpen) emit('close');
    operations.start({
        kind: 'cleanup',
        jiraKey: key,
        title: `Cleaning up ${key}'s worktree`,
        call: (operationId) => store.cleanupWorktree(key, sha, force, operationId),
        success: (snapshot) => ({
            // Cleanup logs its message as a warning when it discarded files or left worker worktrees behind.
            warning: snapshot.events.some((e) => e.level === 'warning' && e.message === snapshot.statusMessage),
            title: `${key}: worktree cleaned up`,
            message: snapshot.statusMessage ?? 'Branch and task artifacts preserved.',
        }),
        failure: (cause) => ({
            title: `${key}: worktree cleanup failed`,
            message: cause instanceof Error ? cause.message : 'Worktree cleanup failed.',
            actions: cause instanceof ApiError && cause.code === BRANCH_UNVERIFIED_CODE
                ? [{
                    label: 'Review',
                    run: () => void router.push({ name: 'task', params: { jiraKey: key }, query: { cleanup: 'force' } }),
                }]
                : [],
        }),
    });
};
</script>

<template>
    <section class="cleanup">
        <template v-if="!startOpen">
            <button v-if="!alreadyClean || confirming || running" :disabled="busy || !!running"
                    @click="confirming ? (confirming = false) : openConfirm()">
                {{ running?.kind === 'cleanup' ? 'Cleaning up worktree…' : 'Clean up worktree' }}
            </button>
            <p v-else class="muted small">Worktree already cleaned up.</p>
        </template>
        <form v-if="confirming" class="alert warn" @submit.prevent="cleanup()">
            <p>
                Remove this task’s local checkout now that it's merged. The branch and task
                artifacts will be kept. Ignored files (build output, dependencies) are also
                cleared but not listed below — there can be tens of thousands of them.
            </p>
            <p v-if="busy && !preview">Checking the worktree…</p>
            <template v-if="previewError">
                <p role="alert">
                    Could not check what would be discarded/closed ({{ previewError }}). Refusing to
                    remove the worktree blind — retry the check first.
                </p>
                <button type="button" class="sm" :disabled="busy" @click="openConfirm(branchUnverified)">Retry check</button>
            </template>
            <template v-if="preview">
                <p v-if="preview.sessionWillBeClosed">
                    <strong>The task's terminal session will be closed</strong>, including its
                    Tests/Shell tabs{{ preview.openAgentPanes.length ? ' and any open agent panes' : '' }}.
                    <template v-if="preview.openAgentPanes.length">
                        Agent pane(s): {{ preview.openAgentPanes.slice(0, MAX_LISTED).join(', ') }}
                        <span v-if="preview.openAgentPanes.length > MAX_LISTED">
                            , +{{ preview.openAgentPanes.length - MAX_LISTED }} more
                        </span>
                    </template>
                    <template v-if="preview.otherOpenTabs.length">
                        Other open tab(s): {{ preview.otherOpenTabs.slice(0, MAX_LISTED).join(', ') }}
                        <span v-if="preview.otherOpenTabs.length > MAX_LISTED">
                            , +{{ preview.otherOpenTabs.length - MAX_LISTED }} more
                        </span>
                    </template>
                </p>
                <p v-else-if="preview.openAgentPanes.length">
                    <strong>{{ preview.openAgentPanes.length }} agent pane(s) will be force-closed:</strong>
                    {{ preview.openAgentPanes.slice(0, MAX_LISTED).join(', ') }}
                    <span v-if="preview.openAgentPanes.length > MAX_LISTED">
                        , +{{ preview.openAgentPanes.length - MAX_LISTED }} more
                    </span>
                </p>
                <p v-if="preview.dirtyPaths.length">
                    <strong>{{ preview.dirtyPaths.length }} modified/untracked file(s) will be
                        permanently discarded; this cannot be undone:</strong>
                    {{ preview.dirtyPaths.slice(0, MAX_LISTED).join(', ') }}
                    <span v-if="preview.dirtyPaths.length > MAX_LISTED">
                        , +{{ preview.dirtyPaths.length - MAX_LISTED }} more
                    </span>
                </p>
                <p v-if="preview.workerWorktrees.length">
                    <strong>{{ preview.workerWorktrees.length }} worker worktree(s) will also be removed:</strong>
                    {{ preview.workerWorktrees.slice(0, MAX_LISTED).join(', ') }}
                    <span v-if="preview.workerWorktrees.length > MAX_LISTED">
                        , +{{ preview.workerWorktrees.length - MAX_LISTED }} more
                    </span>
                </p>
                <p v-if="!preview.sessionWillBeClosed && !preview.openAgentPanes.length && !preview.dirtyPaths.length && !preview.workerWorktrees.length">
                    Worktree is clean, no agent panes are open, and there are no worker worktrees.
                </p>
            </template>
            <template v-if="branchUnverified">
                <p>
                    <strong>AgentFlow can't confirm this task's work was reviewed or merged:</strong>
                    the task branch has commits that weren't in the final review or a merged pull
                    request, or it no longer exists. If you merged it yourself (for example into a
                    local branch), or deleted the branch on purpose, you can remove the worktree
                    anyway. Any branch and its commits are kept; only the checkout (and any
                    uncommitted files listed above) is removed.
                </p>
            </template>
            <div class="row">
                <button class="danger" :disabled="busy || !!running || !expectedSha || !preview">
                    {{ busy ? 'Checking…' : 'Remove worktree' }}
                </button>
                <button v-if="branchUnverified" type="button" class="danger"
                        :disabled="busy || !!running || !expectedSha || !preview" @click="cleanup(true)">
                    Force remove worktree
                </button>
                <button type="button" :disabled="busy" @click="keep">
                    Keep worktree
                </button>
            </div>
        </form>
    </section>
</template>

<style scoped>
.cleanup { margin-block: 0.75rem; }
.row { margin-top: 0.75rem; }
</style>
