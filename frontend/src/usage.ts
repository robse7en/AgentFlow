// One place to turn a UsageSummary into the "tokens · cost · coverage" strings the task
// header, node cards and the execution-evidence panel all show, so they can't drift apart.
import { computed } from 'vue';
import type { UsageSummary } from '@/api/types';
import { formatNzd } from '@/currency';
import { useConfigStore } from '@/stores/config';

const DEFAULT_USD_TO_NZD = 1.65;

export function useUsdToNzdRate() {
    const configStore = useConfigStore();
    return computed(() => Number(configStore.config?.workflow.usdToNzdRate ?? DEFAULT_USD_TO_NZD));
}

/** "~NZ$1.23 (~$0.75 USD)", or "cost unknown" when no run reported a cost. */
export function formatCost(costUsd: number | null | undefined, usdToNzdRate: number): string {
    if (costUsd === null || costUsd === undefined) return 'cost unknown';
    return `${formatNzd(costUsd, usdToNzdRate)} (~$${costUsd.toFixed(2)} USD)`;
}

/** "usage reported for 2/3 runs" when some runs have no usage; empty when coverage is complete. */
export function coverageNote(usage: UsageSummary): string {
    return usage.runsWithUsage < usage.runs ? `usage reported for ${usage.runsWithUsage}/${usage.runs} runs` : '';
}

export function hasUsage(usage: UsageSummary | null | undefined): usage is UsageSummary {
    return !!usage && usage.runsWithUsage > 0;
}

/** Runs with token counts. Copilot reports cost only, so its runs never count here. */
export function tokenRuns(usage: UsageSummary): number {
    return usage.runsWithTokens ?? usage.runsWithUsage;
}

/** " from 2/3 runs" when some runs reported cost only; empty when every reporting run has tokens. */
const tokenScope = (usage: UsageSummary): string =>
    tokenRuns(usage) < usage.runsWithUsage ? ` from ${tokenRuns(usage)}/${usage.runsWithUsage} runs` : '';

/** Compact one-liner for headers: "12,345 tokens · Cost: ~NZ$… · API spend: … (2/3 runs reported)". */
export function usageHeadline(usage: UsageSummary, usdToNzdRate: number): string {
    const coverage = coverageNote(usage);
    const tokens = tokenRuns(usage) ? `${usage.totalTokens.toLocaleString()} tokens${tokenScope(usage)} · ` : '';
    return tokens + withUsageCost(usage, usdToNzdRate)
        + (coverage ? ` (${usage.runsWithUsage}/${usage.runs} runs reported)` : '');
}

/** Fuller breakdown for tooltips: "1,000 in / 200 out · Cost: … · usage reported for 2/3 runs". */
export function usageDetail(usage: UsageSummary, usdToNzdRate: number): string {
    const coverage = coverageNote(usage);
    const tokens = tokenRuns(usage)
        ? `${usage.inputTokens.toLocaleString()} in / ${usage.outputTokens.toLocaleString()} out${tokenScope(usage)} · `
        : '';
    return tokens + withUsageCost(usage, usdToNzdRate) + (coverage ? ` · ${coverage}` : '');
}

const withUsageCost = (usage: UsageSummary, rate: number): string => {
    const cost = usageCostLabel(usage, rate);
    return cost ? `${cost} · ${apiCostLabel(usage, rate)}` : apiCostLabel(usage, rate);
};

/**
 * What the runs consumed whatever the billing mode: provider-billed amounts (e.g. Copilot AI
 * credits) and list-price estimates, never summed together. Empty when no run was priced.
 */
export function usageCostLabel(usage: UsageSummary, rate: number): string {
    const billed = usage.billedCostUsd ?? null;
    const estimated = usage.estimatedCostUsd ?? null;
    if (billed === null && estimated === null) return '';
    const priced = (usage.runsWithBilledCost ?? 0) + (usage.runsWithEstimatedCost ?? 0);
    const partial = priced < usage.runsWithUsage ? ` · ${priced}/${usage.runsWithUsage} runs priced` : '';
    if (estimated === null) return `Cost: ${formatCost(billed, rate)}${partial}`;
    if (billed === null) return `Cost: ${formatCost(estimated, rate)} est.${partial}`;
    return `Cost: ${formatNzd(billed, rate)} billed + ${formatNzd(estimated, rate)} est.${partial}`;
}

/** One run's cost: "~NZ$2.68 (~$1.62 USD) billed by provider", or "… list-price estimate". */
export function runCostLabel(costUsd: number | null | undefined, kind: string | null | undefined, rate: number): string {
    if (costUsd === null || costUsd === undefined) return 'unknown';
    return `${formatCost(costUsd, rate)} ${kind === 'estimated' ? 'list-price estimate' : 'billed by provider'}`;
}

export const apiCostLabel = (usage: UsageSummary, rate: number): string => {
    const unknown = usage.unknownBillingRuns ?? usage.runs;
    const missing = (usage.meteredRuns ?? 0) - (usage.meteredRunsWithCost ?? 0);
    if (!usage.meteredRuns && unknown === 0) return 'subscription · no additional API spend';
    if (usage.apiCostUsd == null || (!usage.meteredRunsWithCost && unknown > 0)) {
        return 'API spend unknown';
    }
    const partial = missing > 0 || unknown > 0;
    return `${partial ? 'Known API spend' : 'API spend'}: ${formatCost(usage.apiCostUsd, rate)}`
        + (partial ? ' · incomplete cost coverage' : '');
};
