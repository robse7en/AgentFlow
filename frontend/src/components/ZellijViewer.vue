<script lang="ts" setup>
import { onBeforeUnmount, ref, watch } from 'vue';

const props = defineProps<{
    jiraKey: string;
    role: string;
}>();

const status = ref('Connecting…');
const inputEnabled = ref(false);
const paneId = ref('');
const input = ref('');
const sending = ref(false);
const keys = [
    { label: 'Enter', value: '\r' },
    { label: 'Tab', value: '\t' },
    { label: 'Up', value: '\x1b[A' },
    { label: 'Down', value: '\x1b[B' },
    { label: 'Left', value: '\x1b[D' },
    { label: 'Right', value: '\x1b[C' },
    { label: 'Escape', value: '\x1b' },
    { label: 'Ctrl+C', value: '\x03' },
];

const sendInput = async (value: string, submit = false): Promise<void> => {
    if (!inputEnabled.value || sending.value || !value) {
        return;
    }
    const currentGeneration = generation;
    sending.value = true;
    error.value = null;
    try {
        const response = await fetch(
            `/api/tasks/${encodeURIComponent(props.jiraKey)}`
                + `/terminal/${encodeURIComponent(props.role)}/input`,
            {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ input: value, pane_id: paneId.value, submit }),
            },
        );
        if (!response.ok) {
            const body = await response.json().catch(() => ({})) as { detail?: string };
            throw new Error(body.detail ?? `Could not send input (${response.status}).`);
        }
        if (currentGeneration === generation && submit) {
            input.value = '';
        }
    } catch (cause) {
        if (currentGeneration === generation) {
            error.value = (cause as Error).message;
        }
    } finally {
        sending.value = false;
    }
};

const screen = ref('');
const error = ref<string | null>(null);
const scrollbackLines = ref<string[]>([]);
const viewportLines = ref<string[]>([]);
let controller: AbortController | null = null;
let generation = 0;

const linesFrom = (value: unknown): string[] => {
    if (Array.isArray(value)) {
        return value.map((line) => String(line));
    }
    return typeof value === 'string' ? value.split('\n') : [];
};

const consumeEvent = (line: string): void => {
    if (!line.trim()) {
        return;
    }

    try {
        const event = JSON.parse(line) as {
            event?: string;
            message?: string;
            viewport?: unknown;
            scrollback?: unknown;
            is_initial?: boolean;
        };

        if (event.event === 'error') {
            error.value = event.message ?? 'The terminal pane stream failed.';
            inputEnabled.value = false;
            status.value = 'Disconnected';
            return;
        }
        if (event.event === 'pane_closed') {
            inputEnabled.value = false;
            status.value = 'Pane closed';
            return;
        }
        if (event.event === 'pane_update') {
            if (event.is_initial) {
                scrollbackLines.value = linesFrom(event.scrollback);
            }
            viewportLines.value = linesFrom(event.viewport);
            screen.value = [...scrollbackLines.value, ...viewportLines.value].join('\n');
            status.value = inputEnabled.value ? 'Live - interactive' : 'Live - read-only';
        }
    } catch {
        inputEnabled.value = false;
        error.value = 'Received invalid data from the terminal pane stream.';
        status.value = 'Disconnected';
    }
};

const connect = async (): Promise<void> => {
    const currentGeneration = ++generation;
    controller?.abort();
    controller = new AbortController();
    screen.value = '';
    inputEnabled.value = false;
    input.value = '';
    scrollbackLines.value = [];
    viewportLines.value = [];
    error.value = null;
    status.value = 'Connecting…';

    try {
        const response = await fetch(
            `/api/tasks/${encodeURIComponent(props.jiraKey)}/terminal/${encodeURIComponent(props.role)}`,
            { signal: controller.signal },
        );
        if (currentGeneration !== generation) {
            return;
        }
        if (!response.ok || !response.body) {
            const body = await response.json().catch(() => ({})) as { detail?: string };
            throw new Error(body.detail ?? `Could not open terminal view (${response.status}).`);
        }

        paneId.value = response.headers.get('x-terminal-pane') ?? '';
        inputEnabled.value = response.headers.get('x-terminal-input') === 'enabled'
            && Boolean(paneId.value);
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let pending = '';
        status.value = inputEnabled.value ? 'Live - interactive' : 'Live - read-only';
        while (true) {
            const { value, done } = await reader.read();
            if (currentGeneration !== generation) {
                return;
            }
            pending += decoder.decode(value, { stream: !done });
            const records = pending.split('\n');
            pending = records.pop() ?? '';
            records.forEach(consumeEvent);
            if (done) {
                if (pending) {
                    consumeEvent(pending);
                }
                inputEnabled.value = false;
                status.value = status.value === 'Pane closed' ? status.value : 'Stream ended';
                break;
            }
        }
    } catch (cause) {
        if (currentGeneration !== generation) {
            return;
        }
        inputEnabled.value = false;
        if (cause instanceof DOMException && cause.name === 'AbortError') {
            return;
        }
        error.value = (cause as Error).message;
        status.value = 'Disconnected';
    }
};

watch(() => [props.jiraKey, props.role], () => void connect(), { immediate: true });
onBeforeUnmount(() => {
    generation++;
    controller?.abort();
});
</script>

<template>
    <section class="zellij-viewer" aria-label="Agent terminal">
        <div class="zellij-viewer__status">
            <span>{{ role.replaceAll('_', ' ') }} · {{ status }}</span>
            <button type="button" class="sm" @click="connect">Reconnect</button>
        </div>
        <div v-if="error" class="alert error small">{{ error }}</div>
        <pre class="zellij-viewer__screen" aria-live="polite">{{ screen || 'Waiting for pane output…' }}</pre>
        <form v-if="inputEnabled" @submit.prevent="sendInput(input, true)">
            <label class="small">Terminal input
                <input v-model="input" type="password" autocomplete="off" maxlength="4000"
                    :disabled="sending" aria-label="Terminal input" />
            </label>
            <button type="submit" class="sm" :disabled="sending">Send + Enter</button>
            <button v-for="key in keys" :key="key.label" type="button" class="sm"
                :disabled="sending" @click="sendInput(key.value)">{{ key.label }}</button>
            <p class="muted small">Input goes directly to this agent's shell. Use these controls
                for trust, login, and interactive prompts. Input is hidden on this form.</p>
        </form>
        <p v-else class="muted small">This view cannot send terminal input. Use the reply controls above to answer an AgentFlow question.</p>
    </section>
</template>

<style scoped>
.zellij-viewer {
    display: grid;
    gap: 0.5rem;
}

.zellij-viewer__status {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    font-size: 0.8rem;
}

.zellij-viewer__screen {
    min-height: 34rem;
    max-height: calc(100vh - 200px);
    background: #000;
    color: #fff;
    white-space: pre;
    word-break: normal;
}
</style>
