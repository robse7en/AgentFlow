<script lang="ts" setup>
import type { HistoricalInstructions } from '@/api/instructions';

defineProps<{ history: HistoricalInstructions; repository?: string }>();
</script>

<template>
    <section aria-label="Historical run instructions">
        <p>Target repository: {{ history.metadata?.repository ?? repository ?? 'Unavailable' }}</p>
        <p>Role: {{ history.metadata?.role ?? 'Legacy run' }}</p>
        <p>Instruction version: {{ history.metadata?.version ?? 'Legacy run; no instruction snapshot' }}</p>
        <p>Revision: {{ history.metadata?.revision ?? 'Unknown' }}</p>
        <p>Source: {{ history.metadata?.source.path ?? 'None' }}</p>
        <p>Briefing SHA-256: {{ history.metadata?.briefingHash ?? 'Unknown' }}</p>
        <pre aria-label="Historical briefing">{{ history.briefing }}</pre>
    </section>
</template>

<style scoped>
p { overflow-wrap: anywhere; }
pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 420px;
    overflow: auto;
    padding: 12px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
}
</style>
