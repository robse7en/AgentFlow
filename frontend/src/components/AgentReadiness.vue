<script lang="ts" setup>
import { computed, ref, watch } from 'vue';
import { useConfigStore } from '@/stores/config';
import { useTaskStore } from '@/stores/tasks';
import { acknowledgedCliSignIns } from '@/cliReadiness';

const props = defineProps<{ harness: string; taskKey: string }>();
const emit = defineEmits<{ ready: [value: boolean] }>();
const config = useConfigStore();
const tasks = useTaskStore();
const busy = ref(false);
const error = ref('');
const selected = computed(() => config.harnessByKey(props.harness));
const available = computed(() => selected.value?.available === true && selected.value.enabled);
const acknowledgementKey = computed(() => JSON.stringify([
    props.harness, selected.value?.resolvedPath, selected.value?.command,
]));
const signedIn = computed({
    get: () => acknowledgedCliSignIns.has(acknowledgementKey.value),
    set: (value: boolean) => {
        if (value) acknowledgedCliSignIns.add(acknowledgementKey.value);
        else acknowledgedCliSignIns.delete(acknowledgementKey.value);
    },
});
const ready = computed(() => available.value && signedIn.value);
const windowsInstall = computed(() => {
    const packages: Record<string, string> = {
        claude: 'Anthropic.ClaudeCode', codex: 'OpenAI.Codex', copilot: 'GitHub.Copilot',
    };
    const packageName = packages[props.harness];
    return packageName ? `winget install --id ${packageName} --exact --source winget` : null;
});
watch(ready, (value) => emit('ready', value), { immediate: true });
const detect = async () => {
    busy.value = true;
    error.value = '';
    try {
        await config.detectHarnesses();
        await tasks.loadTask(props.taskKey);
    } catch (cause) {
        error.value = cause instanceof Error ? cause.message : 'Could not check the CLI.';
    } finally {
        busy.value = false;
    }
};
</script>

<template>
    <section class="readiness alert info" aria-label="Coding CLI readiness">
        <h4>Connect your coding CLI</h4>
        <p>{{ selected?.displayName ?? harness }}: {{ available ? 'executable detected' : 'not ready' }}.</p>
        <p v-if="selected?.message">{{ selected.message }}</p>
        <ol>
            <li>Install this CLI on the computer running AgentFlow using its official installation instructions. If disabled, enable it in the AgentFlow configuration shown in Settings.
                <template v-if="windowsInstall">On Windows: <code>{{ windowsInstall }}</code>.</template>
            </li>
            <li>Open a terminal on that computer, run <code>{{ selected?.command ?? harness }}</code>,
                and complete the CLI sign-in. Authorise any required MCP tools in the CLI.</li>
            <li>Check again below. If installation changed PATH, close and reopen AgentFlow.</li>
        </ol>
        <button type="button" :disabled="busy" @click="detect">
            {{ busy ? 'Checking…' : 'Check CLI again' }}
        </button>
        <p>Account authentication and MCP access are not verified by executable detection.</p>
        <label class="confirmation">
            <input v-model="signedIn" type="checkbox" :disabled="!available" />
            I have signed in to this CLI and authorised the tools this task needs.
        </label>
        <p v-if="error" class="alert error">{{ error }}</p>
    </section>
</template>

<style scoped>
.readiness { overflow-wrap: anywhere; }
h4 { margin-top: 0; }
.confirmation { display: flex; gap: 0.5rem; align-items: flex-start; }
.confirmation input { width: auto; }
</style>
