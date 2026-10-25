<script lang="ts" setup>
import { ref } from 'vue';
import { api } from '@/api/client';
import { useTaskStore } from '@/stores/tasks';

const props = defineProps<{ taskKey: string; runId: number; billingMode?: string }>();
const store = useTaskStore();
const report = ref('');
const billing = ref(props.billingMode ?? 'unknown');
const error = ref('');
const busy = ref(false);
const saved = ref(false);
const save = async (): Promise<void> => {
    busy.value = true;
    error.value = '';
    saved.value = false;
    try {
        const value = JSON.parse(report.value);
        if (!value || typeof value !== 'object' || Array.isArray(value)) {
            throw new Error('Enter a provider report as a JSON object.');
        }
        await api.importUsage(props.taskKey, props.runId, { ...value, billingMode: billing.value });
        await store.loadTask(props.taskKey);
        saved.value = true;
    } catch (cause) {
        error.value = cause instanceof Error ? cause.message : String(cause);
    } finally { busy.value = false; }
};
</script>

<template>
    <details>
        <summary>Import or correct provider usage</summary>
        <p class="small muted">Use measured totals for this run only. This replaces its current
            report. Omit costUsd when unknown; do not enter zero for missing data.</p>
        <form @submit.prevent="save">
            <label class="field"><span>Provider report JSON</span>
                <textarea v-model="report" required rows="5" maxlength="16000"
                    placeholder='{"inputTokens":100,"outputTokens":20,"cachedInputTokens":0,"model":"provider model","source":"provider report","costUsd":0.02}' />
            </label>
            <label class="field"><span>How this run was billed</span><select v-model="billing">
                <option value="unknown">Unknown</option><option value="metered">Metered API</option>
                <option value="subscription">Subscription / unlimited</option>
            </select></label>
            <button :disabled="busy">Replace usage report</button>
            <p v-if="error" class="alert error">{{ error }}</p>
            <p v-if="saved" class="alert ok" role="status">Usage report saved.</p>
        </form>
    </details>
</template>

<style scoped>
textarea { font-family: var(--mono); }
</style>
