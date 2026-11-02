// Wording for cost estimates from past runs (see backend/agentflow/workflow/estimates.py). They are
// display only: budgets and launch checks never use them.
import type { CostEstimate, EstimateTotals } from '@/api/types';
import { formatNzd } from '@/currency';

const tokens = (value: number): string =>
    value >= 1_000_000 ? `${(value / 1_000_000).toFixed(1)}M` : value >= 1000 ? `${Math.round(value / 1000)}k` : String(value);

const usd = (value: number, rate: number): string => `${formatNzd(value, rate)} (~$${value.toFixed(2)} USD)`;

/** "list-price estimate" / "partly list-price" for figures that are not all provider-billed. */
const kindNote = (estimate: CostEstimate): string =>
    estimate.costKind === 'estimated' ? ', list-price estimate' : estimate.costKind === 'mixed' ? ', partly list-price' : '';

/** "Typical run ~NZ$1.02 (~$0.62 USD), up to … (12 past runs)", or why there is no estimate yet. */
export function describeEstimate(estimate: CostEstimate, rate: number): string {
    if (!estimate.enough || estimate.typical === null || estimate.high === null) {
        const runs = estimate.runs === 1 ? '1 past run' : `${estimate.runs} past runs`;
        return `No cost estimate yet (${runs} with this harness and model; needs 3)`;
    }
    const runs = `${estimate.runs} past runs`;
    if (estimate.unit === 'tokens') {
        return `Typical run ${tokens(estimate.typical)} tokens, up to ${tokens(estimate.high)} (subscription; ${runs})`;
    }
    return `Typical run ${usd(estimate.typical, rate)}, up to ${usd(estimate.high, rate)} (${runs}${kindNote(estimate)})`;
}

/** "~NZ$… (~$… USD), up to … + 120k subscription tokens", or empty when nothing is known. */
export function describeTotals(totals: EstimateTotals | null | undefined, rate: number): string {
    if (!totals) return '';
    const parts: string[] = [];
    if (totals.usd) parts.push(`${usd(totals.usd.typical, rate)}, up to ${usd(totals.usd.high, rate)}`);
    if (totals.tokens) parts.push(`${tokens(totals.tokens.typical)} subscription tokens, up to ${tokens(totals.tokens.high)}`);
    return parts.join(' + ');
}
