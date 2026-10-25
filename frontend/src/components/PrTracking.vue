<script lang="ts" setup>
import { computed, ref } from 'vue';
import { useTaskStore } from '@/stores/tasks';
import { useConfigStore } from '@/stores/config';
import type { TaskSnapshot } from '@/api/types';

const props = defineProps<{ task: TaskSnapshot }>();
const store = useTaskStore();
const configStore = useConfigStore();
const githubSync = computed(() => configStore.config?.github?.prSync ?? false);
const pollMinutes = computed(() => Math.max(1, Math.round((configStore.config?.github?.pollIntervalSeconds ?? 120) / 60)));
const busy = ref(false);
const error = ref<string | null>(null);
const prUrl = ref('');
const confirmingRevert = ref(false);
// GitHub's new-PR form after a push. Opening it from code after the push may be blocked as a
// pop-up (the click is long past), so it is always offered as a link too.
const formUrl = ref<string | null>(null);

// Most PRs go to the repository's default branch; anything else is easy to miss on GitHub.
const unusualBase = computed(() => !['main', 'master'].includes(props.task.baseBranch));
const wrongBase = computed(() => !!props.task.prBaseBranch && props.task.prBaseBranch !== props.task.baseBranch);

// How far along Ready -> PR created -> Merged the task is (0: not there yet).
const STEPS = ['Ready', 'PR created', 'Merged'];
const reached = computed(() => ({ READY_FOR_PR: 1, PR_CREATED: 2, MERGED: 3 } as Record<string, number>)[props.task.status] ?? 0);
const STATUS_TEXT: Record<string, string> = { READY_FOR_PR: 'Ready', PR_CREATED: 'Open', MERGED: 'Merged' };
// Cancelling leaves any PR as it was, so an ended task may still have one.
const ended = computed(() => props.task.status === 'CANCELLED' || props.task.status === 'FAILED');
const pillText = computed(() => STATUS_TEXT[props.task.status]
    ?? (ended.value ? (props.task.prUrl ? 'PR opened' : 'No PR') : 'Not ready'));

async function act(fn: () => Promise<unknown>) {
    busy.value = true;
    error.value = null;
    try {
        await fn();
        confirmingRevert.value = false;
    } catch (e) {
        error.value = e instanceof Error ? e.message : 'Action failed.';
    } finally {
        busy.value = false;
    }
}

const create = () => act(() => store.markPrCreated(props.task.jiraKey, prUrl.value.trim()));
const revertCreated = () => act(() => store.revertPrCreated(props.task.jiraKey));
const merge = () => act(() => store.markPrMerged(props.task.jiraKey));
const revertMerged = () => act(() => store.revertPrMerged(props.task.jiraKey));
const retarget = () => act(() => store.retargetPr(props.task.jiraKey));
const openPr = () => act(async () => {
    formUrl.value = await store.openPr(props.task.jiraKey);
    window.open(formUrl.value, '_blank', 'noopener,noreferrer');
});
</script>

<template>
    <section class="pr-tracking panel strip-card" aria-label="Pull request">
        <div class="panel-title">
            <h2>Pull request</h2>
            <span class="status-pill" :class="{ done: reached > 0 }">{{ pillText }}</span>
        </div>
        <ol class="steps" aria-label="Pull request progress">
            <template v-for="(step, index) in STEPS" :key="step">
                <li :class="{ on: index < reached }"><span class="dot" aria-hidden="true" />{{ step }}</li>
                <li v-if="index < STEPS.length - 1" class="line" aria-hidden="true" />
            </template>
        </ol>
        <p class="target small" :class="{ unusual: unusualBase }">
            <code>{{ task.branchName }}</code> <span aria-label="into">→</span> <code class="base">{{ task.baseBranch }}</code>
        </p>
        <div v-if="error" class="alert error small">{{ error }}</div>
        <div v-if="wrongBase" class="alert warn small">
            This PR targets <code>{{ task.prBaseBranch }}</code>, but the task started from <code>{{ task.baseBranch }}</code>.
            <template v-if="task.status === 'PR_CREATED'">
                <button class="sm" :disabled="busy" @click="retarget">Retarget to {{ task.baseBranch }}</button>
            </template>
        </div>
        <p v-if="ended && task.prUrl" class="small">
            The task ended with a PR open:
            <a :href="task.prUrl" target="_blank" rel="noopener noreferrer">{{ task.prUrl }}</a>
        </p>
        <p v-else-if="ended" class="small muted">
            This task ended before reaching a pull request.
        </p>
        <p v-else-if="reached === 0" class="small muted">
            Available once the task is Ready for PR: you push <code>{{ task.branchName }}</code> and open the PR.
        </p>
        <p v-else-if="githubSync && task.prSyncPaused && (task.status === 'READY_FOR_PR' || task.status === 'PR_CREATED')" class="small muted">
            GitHub sync is paused for this task because a PR status was undone. Marking the PR yourself turns it back on.
        </p>
        <p v-else-if="githubSync && task.status === 'READY_FOR_PR'" class="small muted">
            Once a PR is opened from <code>{{ task.branchName }}</code>, AgentFlow picks it up from GitHub within about {{ pollMinutes }} min.
        </p>
        <p v-else-if="githubSync && task.status === 'PR_CREATED'" class="small muted">
            AgentFlow marks this Merged when GitHub shows the PR merged (checked about every {{ pollMinutes }} min).
        </p>

        <template v-if="task.status === 'READY_FOR_PR'">
            <div class="row">
                <button class="primary" :disabled="busy" @click="openPr">Push &amp; open PR into {{ task.baseBranch }}</button>
                <a v-if="formUrl" :href="formUrl" target="_blank" rel="noopener noreferrer" class="small">Open the PR form again</a>
            </div>
            <p v-if="formUrl" class="small muted">
                Pushed. GitHub's form has <code>{{ task.baseBranch }}</code> chosen as the target.
            </p>
            <form class="input-row" @submit.prevent="create">
                <label class="sr-only" for="pr-url">Pull request URL</label>
                <input id="pr-url" v-model="prUrl" type="url" placeholder="https://github.com/org/repo/pull/123" required :disabled="busy" />
                <button class="success" :disabled="busy || !prUrl.trim()">Mark PR created</button>
            </form>
        </template>

        <template v-else-if="task.status === 'PR_CREATED'">
            <p>
                PR created:
                <a :href="task.prUrl ?? undefined" target="_blank" rel="noopener noreferrer">{{ task.prUrl }}</a>
            </p>
            <div class="row">
                <button class="sm success" :disabled="busy" @click="merge">Mark PR merged</button>
                <button v-if="!confirmingRevert" class="sm" :disabled="busy" @click="confirmingRevert = true">Undo…</button>
                <template v-else>
                    <span class="small muted">Back to Ready for PR?</span>
                    <button class="sm danger" :disabled="busy" @click="revertCreated">Yes, undo</button>
                    <button class="sm" :disabled="busy" @click="confirmingRevert = false">Cancel</button>
                </template>
            </div>
        </template>

        <template v-else-if="task.status === 'MERGED'">
            <p>
                Merged<span v-if="task.prUrl">: <a :href="task.prUrl ?? undefined" target="_blank" rel="noopener noreferrer">{{ task.prUrl }}</a></span>
            </p>
            <div class="row">
                <button v-if="!confirmingRevert" class="sm" :disabled="busy" @click="confirmingRevert = true">Undo…</button>
                <template v-else>
                    <span class="small muted">Back to PR Created?</span>
                    <button class="sm danger" :disabled="busy" @click="revertMerged">Yes, undo</button>
                    <button class="sm" :disabled="busy" @click="confirmingRevert = false">Cancel</button>
                </template>
            </div>
        </template>
    </section>
</template>

<style scoped>
.row { margin-top: 0.5rem; display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem; }
.row + .input-row { margin-top: 0.5rem; }
.target { margin: 0 0 0.5rem; color: var(--muted); overflow-wrap: anywhere; }
.target.unusual .base { background: var(--warn-soft); color: var(--warn); font-weight: 600; }
.alert.warn button { margin-left: 0.5rem; }
.steps { display: flex; flex-wrap: wrap; align-items: center; gap: 0.35rem 0.5rem; margin: 0 0 0.75rem; padding: 0; list-style: none; font-size: 12px; color: var(--muted); }
.steps li { display: inline-flex; align-items: center; gap: 0.4rem; white-space: nowrap; }
.steps li.on { color: var(--text); }
.steps .dot { width: 10px; height: 10px; margin: 0; border-radius: 50%; background: var(--border-strong); }
.steps li.on .dot { background: var(--ok); }
.steps .line { flex: 1; min-width: 12px; height: 2px; border-radius: 2px; background: var(--border); }
</style>
