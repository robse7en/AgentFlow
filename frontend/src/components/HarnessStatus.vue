<script setup lang="ts">
import { computed } from 'vue'
import { useConfigStore } from '@/stores/config'

const config = useConfigStore()
const system = computed(() => config.config?.system)
</script>

<template>
  <div class="panel">
    <div class="panel-title">
      <h3>System</h3>
      <button class="sm" @click="config.detectHarnesses()">Re-detect</button>
    </div>
    <div v-if="!config.config" class="muted">Loading…</div>
    <template v-else>
      <ul class="check-list">
        <li :class="system?.git.available ? 'ok' : 'bad'">Git <span class="muted small">{{ system?.git.version ?? 'not found' }}</span></li>
        <li :class="system?.terminal.available ? 'ok' : 'bad'">Terminal <span class="muted small">{{ system?.terminal.version ?? 'zellij not found — required to run agents' }}</span></li>
      </ul>
      <h4 style="margin-top: 0.9rem">Harnesses</h4>
      <ul class="check-list">
        <li v-for="h in config.harnesses" :key="h.key" :class="h.available && h.enabled ? 'ok' : 'bad'">
          {{ h.displayName }}
          <span class="muted small">
            <template v-if="!h.enabled">disabled</template>
            <template v-else-if="!h.available">unavailable — {{ h.message }}</template>
            <template v-else>{{ h.version ?? h.resolvedPath }}</template>
          </span>
        </li>
      </ul>
    </template>
  </div>
</template>
