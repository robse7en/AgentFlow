<script lang="ts" setup>
import { reactive, ref, watch } from 'vue';
import { useConfigStore } from '@/stores/config';

const store = useConfigStore();
const form = reactive({ taskBudgetUsd: '', dailyBudgetUsd: '', launchReserveUsd: '1',
    warningPercent: '80', copilotRoles: [] as string[], requireMeteredApproval: false });
const billing = reactive<Record<string, string>>({});
const busy = ref(false);
const error = ref('');
const saved = ref(false);
const roles = ['architect', 'orchestrator', 'implementer', 'code_reviewer', 'security_reviewer'];
watch(() => store.config, (config) => {
    if (!config) return;
    const controls = config.workflow.costControls as Record<string, unknown> | undefined;
    form.taskBudgetUsd = String(controls?.taskBudgetUsd ?? '');
    form.dailyBudgetUsd = String(controls?.dailyBudgetUsd ?? '');
    form.launchReserveUsd = String(controls?.launchReserveUsd ?? 1);
    form.warningPercent = String(controls?.warningPercent ?? 80);
    form.copilotRoles = [...(controls?.copilotRoles as string[] ?? [])];
    form.requireMeteredApproval = controls?.requireMeteredApproval === true;
    for (const harness of config.harnesses) billing[harness.key] = harness.billingMode ?? 'unknown';
}, { immediate: true });
const save = async (): Promise<void> => {
    busy.value = true;
    error.value = '';
    saved.value = false;
    try {
        await store.setCostControls({ ...form,
            taskBudgetUsd: form.taskBudgetUsd === '' ? null : Number(form.taskBudgetUsd),
            dailyBudgetUsd: form.dailyBudgetUsd === '' ? null : Number(form.dailyBudgetUsd),
            launchReserveUsd: Number(form.launchReserveUsd), warningPercent: Number(form.warningPercent),
        }, billing);
        saved.value = true;
    } catch (cause) {
        error.value = cause instanceof Error ? cause.message : String(cause);
    } finally { busy.value = false; }
};
</script>

<template>
    <form class="panel" @submit.prevent="save">
        <h3>API spend controls</h3>
        <p class="small muted">USD limits apply to new launches. Running agents can exceed their
            reservations. Missing costs block new metered launches when a limit is enabled.</p>
        <div class="grid-2">
            <label class="field"><span>Default ticket limit (USD; blank disables)</span>
                <input v-model="form.taskBudgetUsd" type="number" min="0.01" step="0.01" /></label>
            <label class="field"><span>Shared daily limit (USD; UTC day; blank disables)</span>
                <input v-model="form.dailyBudgetUsd" type="number" min="0.01" step="0.01" /></label>
            <label class="field"><span>Reserve per active API run (USD)</span>
                <input v-model="form.launchReserveUsd" type="number" min="0.01" step="0.01" required /></label>
            <label class="field"><span>Warn at percentage</span>
                <input v-model="form.warningPercent" type="number" min="1" max="100" required /></label>
        </div>
        <h4>Billing by harness</h4>
        <p class="small muted">Classifications apply to future runs. Historical unclassified reports
            remain unknown. Subscription tokens remain visible and contribute no additional API spend.</p>
        <label v-for="harness in store.config?.harnesses" :key="harness.key" class="field">
            <span>{{ harness.displayName }}</span>
            <select v-model="billing[harness.key]">
                <option value="unknown">Unknown</option><option value="metered">Metered API</option>
                <option value="subscription">Subscription / unlimited</option>
            </select>
        </label>
        <h4>Prefer Copilot for these roles</h4>
        <p class="small muted">Uses the configured Copilot model when available. Explicit task and
            stage overrides take precedence. A metered fallback always needs approval.</p>
        <label v-for="role in roles" :key="role" class="role-choice">
            <input v-model="form.copilotRoles" type="checkbox" :value="role" /> {{ role.replaceAll('_', ' ') }}
        </label>
        <label class="role-choice"><input v-model="form.requireMeteredApproval" type="checkbox" />
            Require a reason and approval before each metered or unclassified launch</label>
        <p v-if="error" class="alert error">{{ error }}</p>
        <p v-if="saved" class="alert ok" role="status">Cost controls saved.</p>
        <button class="primary" :disabled="busy">{{ busy ? 'Saving…' : 'Save cost controls' }}</button>
    </form>
</template>

<style scoped>
.role-choice { display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.7rem; }
.role-choice input { width: auto; }
</style>
