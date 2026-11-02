<script lang="ts" setup>
// The task header's pull-request step, so the next PR action is never buried below the canvas:
// Push & open PR -> Mark PR created -> PR link + Mark merged. PrTracking keeps the detail and undo.
import { computed, ref } from 'vue';
import { useTaskStore } from '@/stores/tasks';
import { useConfigStore } from '@/stores/config';
import Modal from '@/components/Modal.vue';
import type { TaskSnapshot } from '@/api/types';

const props = defineProps<{ task: TaskSnapshot }>();
const store = useTaskStore();
const configStore = useConfigStore();
const githubSync = computed(() => configStore.config?.github?.prSync ?? false);
const busy = ref(false);
const error = ref<string | null>(null);
// GitHub's new-PR form, once pushed. Opening it after an await may be blocked as a pop-up,
// so it stays on offer as a link.
const formUrl = ref<string | null>(null);
const marking = ref(false);
const prUrl = ref('');

// Most PRs go to the repository's default branch; anything else is easy to miss on GitHub.
const unusualBase = computed(() => !['main', 'master'].includes(props.task.baseBranch));
const prLabel = computed(() => {
    const number = props.task.prUrl?.match(/\/pull\/(\d+)/)?.[1];
    return number ? `PR #${number}` : 'View PR';
});

async function act(fn: () => Promise<unknown>) {
    busy.value = true;
    error.value = null;
    try {
        await fn();
    } catch (e) {
        error.value = e instanceof Error ? e.message : 'Action failed.';
    } finally {
        busy.value = false;
    }
}

const openPr = () => act(async () => {
    formUrl.value = await store.openPr(props.task.jiraKey);
    window.open(formUrl.value, '_blank', 'noopener,noreferrer');
});
const markCreated = () => act(async () => {
    await store.markPrCreated(props.task.jiraKey, prUrl.value.trim());
    marking.value = false;
    prUrl.value = '';
});
const merge = () => act(() => store.markPrMerged(props.task.jiraKey));
const closeMarking = () => {
    marking.value = false;
    error.value = null;
};
</script>

<template>
    <template v-if="task.status === 'READY_FOR_PR'">
        <button v-if="!formUrl" type="button" class="primary pr-cta" :disabled="busy"
                :title="`Push ${task.branchName} and open GitHub's pull request form into ${task.baseBranch}`" @click="openPr">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="6" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" /><circle cx="18" cy="18" r="2.5" /><path d="M6 8.5v7" /><path d="M18 15.5V9a3 3 0 0 0-3-3h-4" /><path d="m13 3-3 3 3 3" /></svg>
            {{ busy ? 'Pushing…' : 'Push & open PR' }}
            <span class="into" :class="{ unusual: unusualBase }"><span aria-label="into">→</span> {{ task.baseBranch }}</span>
        </button>
        <template v-else>
            <a class="btn" :href="formUrl" target="_blank" rel="noopener noreferrer"
               :title="`GitHub's form, with ${task.baseBranch} chosen as the target`">PR form ↗</a>
            <button type="button" class="primary" :disabled="busy"
                    :title="githubSync ? 'GitHub sync also picks the PR up on its own within a few minutes' : undefined"
                    @click="marking = true">Mark PR created</button>
        </template>
    </template>
    <template v-else-if="task.status === 'PR_CREATED'">
        <a v-if="task.prUrl" class="btn" :href="task.prUrl" target="_blank" rel="noopener noreferrer">{{ prLabel }} ↗</a>
        <button type="button" class="success" :disabled="busy" @click="merge">Mark merged</button>
    </template>
    <span v-if="error && !marking" class="pr-error small" role="alert">{{ error }}</span>

    <Modal v-if="marking" title="Mark PR created" narrow @close="closeMarking">
        <form class="mark-form" @submit.prevent="markCreated">
            <label class="small muted" for="header-pr-url">Pull request URL</label>
            <input id="header-pr-url" v-model="prUrl" type="url" placeholder="https://github.com/org/repo/pull/123"
                   required autofocus :disabled="busy" />
            <div v-if="error" class="alert error small">{{ error }}</div>
            <div class="mark-actions">
                <button type="button" class="sm" :disabled="busy" @click="closeMarking">Cancel</button>
                <button type="submit" class="sm success" :disabled="busy || !prUrl.trim()">Mark PR created</button>
            </div>
        </form>
    </Modal>
</template>

<style scoped>
.pr-cta .into { font-family: var(--mono); font-size: 12.5px; font-weight: 600; opacity: 0.9; }
.pr-cta .into.unusual { padding: 0.05rem 0.4rem; border-radius: var(--radius-sm); background: var(--warn-soft); color: var(--warn); opacity: 1; }
a.btn { min-height: 44px; padding-inline: 1.2rem; }
.pr-error { flex-basis: 100%; color: var(--danger); overflow-wrap: anywhere; }
.mark-form { display: flex; flex-direction: column; gap: 0.5rem; }
.mark-actions { display: flex; justify-content: flex-end; gap: 0.5rem; }
</style>
