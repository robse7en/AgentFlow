<script lang="ts" setup>
import type { TaskSnapshot } from '@/api/types';
import { formatNzd } from '@/currency';
import { apiCostLabel, hasUsage, runCostLabel, tokenRuns, usageCostLabel, useUsdToNzdRate } from '@/usage';
import UsageImport from '@/components/UsageImport.vue';

const props = defineProps<{ task: TaskSnapshot }>();
const usdToNzdRate = useUsdToNzdRate();
const nodeLabel = (key: string): string => props.task.nodes.find((n) => n.key === key)?.label ?? key.replaceAll('_', ' ');
</script>

<template>
    <section class="panel strip-card" aria-label="Execution cost and verification">
        <div class="panel-title">
            <h2>Execution cost &amp; verification</h2>
            <span class="status-pill">{{ task.totalUsage?.runsWithUsage ?? 0 }} of {{ task.totalUsage?.runs ?? 0 }} runs</span>
        </div>
        <p>
            Usage reported for {{ task.totalUsage?.runsWithUsage ?? 0 }} of
            {{ task.totalUsage?.runs ?? 0 }} runs. Missing usage and cost are unknown.
        </p>
        <p v-if="hasUsage(task.totalUsage)">
            Total across the flow:
            <template v-if="tokenRuns(task.totalUsage)">
                {{ task.totalUsage.totalTokens.toLocaleString() }} tokens
                ({{ task.totalUsage.inputTokens.toLocaleString() }} in / {{ task.totalUsage.outputTokens.toLocaleString() }} out) ·
            </template>
            {{ apiCostLabel(task.totalUsage, usdToNzdRate) }}.
        </p>
        <p v-if="hasUsage(task.totalUsage) && tokenRuns(task.totalUsage) < task.totalUsage.runsWithUsage">
            Copilot reports cost only, so token totals cover {{ tokenRuns(task.totalUsage) }} of
            {{ task.totalUsage.runsWithUsage }} runs with usage.
        </p>
        <p v-if="hasUsage(task.totalUsage) && usageCostLabel(task.totalUsage, usdToNzdRate)">
            {{ usageCostLabel(task.totalUsage, usdToNzdRate) }}. Billed amounts are what the provider
            metered (e.g. Copilot AI credits, including subscription allowance); estimates price tokens
            at public list rates. Neither counts toward API spend limits.
        </p>
        <details v-for="run in task.executionEvidence ?? []" :key="run.runId" class="run">
            <summary>
                <span v-if="run.verification" class="status-pill" :class="run.verification.passed ? 'done' : 'attention'">
                    {{ run.verification.passed ? 'Verified' : 'Unverified' }}
                </span>
                {{ nodeLabel(run.nodeKey) }} · Run {{ run.runId }} · {{ Math.round(run.durationSeconds) }} seconds ·
                {{ !run.usage ? 'usage unknown' : run.usage.totalTokens != null ? `${run.usage.totalTokens} tokens`
                    : formatNzd(run.usage.costUsd, usdToNzdRate) }}
            </summary>
            <p>Requested model: {{ run.requestedModel ?? 'harness default' }}.</p>
            <p>Reported runtime model: {{ run.confirmedModel ?? 'unknown' }}.</p>
            <p v-if="run.billingMode === 'subscription'">
                Subscription usage; no additional API spend.
                Cost: {{ runCostLabel(run.usage?.costUsd, run.usage?.costKind, usdToNzdRate) }}.
            </p>
            <p v-else>
                {{ run.billingMode === 'metered' ? 'API cost' : 'Reported cost (billing unclassified)' }}:
                {{ runCostLabel(run.usage?.costUsd, run.usage?.costKind, usdToNzdRate) }}.
            </p>
            <p v-if="run.usage">Usage source: {{ run.usage.provenance }}.</p>
            <UsageImport :key="`${run.runId}-${run.billingMode}`" :task-key="task.jiraKey"
                :run-id="run.runId" :billing-mode="run.billingMode" />
            <template v-if="run.verification">
                <p>Checkpoint: <code>{{ run.verification.sha }}</code></p>
                <p>Independent verification: {{ run.verification.passed ? 'passed' : 'unverified or failed' }}</p>
                <p v-if="run.verification.error">{{ run.verification.error }}</p>
                <details v-for="(command, index) in run.verification.commands ?? []" :key="index">
                    <summary>{{ command.command.join(' ') }} — exit {{ command.exitCode }}</summary>
                    <pre>{{ command.output }}</pre>
                </details>
            </template>
        </details>
    </section>
</template>

<style scoped>
pre { white-space: pre-wrap; overflow-wrap: anywhere; }
details { margin-block: 0.5rem; }
/* Each run collapses to one row: verification output can run long. */
.run { border-radius: var(--radius-md); background: var(--info-soft); overflow: hidden; }
.run > summary {
    display: flex; align-items: center; gap: 0.6rem; min-height: 44px; padding: 0.4rem 0.9rem;
    cursor: pointer; font-size: 13px; list-style: none; transition: background 0.25s;
}
.run > summary::-webkit-details-marker { display: none; }
.run > summary::after {
    content: ""; flex: none; margin-left: auto; width: 7px; height: 7px;
    border-right: 2px solid var(--muted); border-bottom: 2px solid var(--muted);
    transform: rotate(45deg); transition: transform 0.35s var(--spring);
}
.run[open] > summary::after { transform: rotate(225deg); }
.run > summary:hover { background: var(--hover); }
.run > :not(summary) { margin-inline: 0.9rem; font-size: 13px; }
.run > :last-child { margin-bottom: 0.9rem; }
@media (prefers-reduced-motion: reduce) { .run > summary::after { transition: none; } }
</style>
