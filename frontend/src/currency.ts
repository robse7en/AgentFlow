// Display-only conversion. Run costs are reported and budgeted in USD (see
// backend/agentflow/workflow/usage.py and engine.py); the rate itself lives in
// workflow.usdToNzdRate (Settings > Workflow) since it's a fixed approximation,
// not a live FX lookup, and drifts over time.
export function formatNzd(usd: number | null | undefined, usdToNzdRate: number): string {
    if (usd === null || usd === undefined) return 'unknown';
    return `~NZ$${(usd * usdToNzdRate).toFixed(2)}`;
}
