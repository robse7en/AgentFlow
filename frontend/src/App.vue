<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useTaskStore } from '@/stores/tasks'
import { useConfigStore } from '@/stores/config'
import { useProjectStore } from '@/stores/projects'
import { useDecisionStore } from '@/stores/decisions'
import ToastHost from '@/components/ToastHost.vue'
import AppSidebar from '@/components/AppSidebar.vue'

const tasks = useTaskStore()
const config = useConfigStore()
const projects = useProjectStore()
const decisions = useDecisionStore()
const router = useRouter()

const isDesktop = (window as Window & { __AGENTFLOW_DESKTOP__?: boolean })
    .__AGENTFLOW_DESKTOP__ === true;
const retryDesktop = () => window.location.assign('/__desktop/reconnect');
// Live updates start disconnected; warn only once they have connected and then dropped.
const liveUpdatesDropped = ref(false);
let liveUpdatesConnected = false;
watch(() => tasks.connected, (connected) => {
    if (connected) liveUpdatesConnected = true;
    liveUpdatesDropped.value = liveUpdatesConnected && !connected;
});
const paired = ref(false);
// Until the first session check and any pairing link are done, the desktop is still connecting.
const starting = ref(true);
const code = ref('');
const error = ref('');
const busy = ref(false);

const start = () => {
    paired.value = true;
    tasks.connect();
    void config.load();
    void projects.load();
    // The sidebar's waiting count; it follows the live task feed from here on.
    decisions.start();
};

const unpair = () => {
    paired.value = false;
    tasks.stopConnection();
};

const pair = async (value = code.value) => {
    busy.value = true;
    error.value = '';
    try {
        const response = await fetch('/api/auth/pair', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ code: value.trim() }),
        });
        if (!response.ok) throw new Error((await response.json()).detail);
        code.value = '';
        start();
    } catch (cause) {
        error.value = (cause as Error).message;
    } finally {
        busy.value = false;
    }
};

// The backend prints a link carrying the code in the fragment, which never reaches server logs.
// It may arrive on first load or be pasted into an open tab (a hash-only navigation).
let sessionChecked = false;
const pairFromLink = async () => {
    const linked = new URLSearchParams(router.currentRoute.value.hash.slice(1)).get('pair');
    if (!sessionChecked || linked === null) return;
    await router.replace({ ...router.currentRoute.value, hash: '' });
    if (!paired.value && linked) await pair(linked);
};
watch(() => router.currentRoute.value.hash, pairFromLink);

onMounted(async () => {
    window.addEventListener('agentflow:unpaired', unpair);
    try {
        if ((await fetch('/api/auth/session')).ok) start();
    } catch {
        error.value = isDesktop
            ? 'Could not reach the local AgentFlow service. Retry the connection below.'
            : 'Could not reach AgentFlow. Check the backend terminal.';
    }
    sessionChecked = true;
    try {
        await router.isReady();
        await pairFromLink();
    } finally {
        starting.value = false;
    }
});
onBeforeUnmount(() => {
    window.removeEventListener('agentflow:unpaired', unpair);
    tasks.stopConnection();
});
</script>

<template>
  <div class="app-shell">
    <AppSidebar />
    <main class="page">
      <template v-if="paired">
        <section v-if="isDesktop && liveUpdatesDropped" class="alert info">
          <p>Live updates are disconnected. AgentFlow is reconnecting automatically.</p>
          <button @click="retryDesktop">Retry connection</button>
          <a href="/api/diagnostics" download>Export diagnostics</a>
        </section>
        <RouterView />
      </template>
      <p v-else-if="isDesktop && starting" class="muted">Connecting…</p>
      <section v-else-if="isDesktop" class="panel">
        <h2>Reconnect AgentFlow</h2>
        <p>The desktop connection is unavailable. Retry to reconnect without stopping running agents.</p>
        <p v-if="error" class="alert error">{{ error }}</p>
        <button class="primary" @click="retryDesktop">Retry connection</button>
        <p class="small muted">If retry fails, close and reopen AgentFlow. Closing with active agents requires confirmation before stopping them.</p>
      </section>
      <form v-else class="panel" @submit.prevent="pair()">
        <h2>Connect this browser</h2>
        <p>Open the pairing link shown in the AgentFlow backend terminal, or enter its one-time code.</p>
        <label class="field">
          <span>Pairing code</span>
          <input v-model="code" type="password" autocomplete="off" required />
        </label>
        <p v-if="error" class="alert error">{{ error }}</p>
        <button class="primary" :disabled="busy">Connect</button>
      </form>
    </main>
    <ToastHost />
  </div>
</template>
