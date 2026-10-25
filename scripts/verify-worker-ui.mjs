// Local rendering check; uses installed frontend dependencies and makes no network requests.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(new URL('../frontend/package.json', import.meta.url));
const { createServer } = require('vite');
const root = fileURLToPath(new URL('../frontend', import.meta.url));
const server = await createServer({ root, server: { middlewareMode: true }, appType: 'custom' });

try {
    const { default: Workers } = await server.ssrLoadModule('/src/components/ImplementerWorkers.vue');
    const { createSSRApp } = require('vue');
    const { renderToString } = require('@vue/server-renderer');
    // Match the ESM entry used by Vite's externalized store import.
    const { createPinia, setActivePinia } = await import(
        new URL('../frontend/node_modules/pinia/dist/pinia.mjs', import.meta.url).href
    );
    const states = ['RUNNING', 'FAILED', 'INTERRUPTED', 'COMPLETED'];
    const task = {
        jiraKey: 'LOCAL-UI',
        nodes: states.map((state, index) => ({
            key: `worker_${index}`,
            label: `<script>worker ${index}</script>`,
            state,
            description: 'A focused task',
            worker: {
                uid: String(index), key: `part-${index}`, ownedPaths: [`src/part-${index}`],
                acceptanceCriteria: ['Required behavior'], dependencies: [],
                worktreePath: `local/worker-${index}`, branchName: `worker-${index}`,
            },
        })),
        sessions: [{ nodeKey: 'worker_0', status: 'running', paneId: 'terminal_1' }],
    };
    const app = createSSRApp(Workers, { task });
    const pinia = createPinia();
    setActivePinia(pinia);
    app.use(pinia);
    const html = await renderToString(app);
    assert.equal((html.match(/<details/g) ?? []).length, 4);
    assert.match(html, /Show Zellij terminal/);
    assert.match(html, /Retry/);
    assert.match(html, /Resume existing agent/);
    assert.match(html, /Stop &amp; retry/);
    assert.match(html, /&lt;script&gt;worker 0&lt;\/script&gt;/);
    assert.ok(!html.includes('<script>worker'));
    console.log('Worker UI rendering passed: four states, terminal/retry/recovery controls, escaped text.');
    const { default: Evidence } = await server.ssrLoadModule('/src/components/ExecutionEvidence.vue');
    const evidenceHtml = await renderToString(createSSRApp(Evidence, { task: {
        jiraKey: 'LOCAL-UI', nodes: [{ key: 'implementer', label: 'Implementer' }],
        totalUsage: { runs: 1, runsWithUsage: 0, inputTokens: 0, outputTokens: 0, cachedInputTokens: 0, totalTokens: 0, costUsd: null },
        executionEvidence: [{ runId: 1, nodeKey: 'implementer', durationSeconds: 2, usage: null,
            requestedModel: null, confirmedModel: null,
            verification: { sha: 'abc', passed: false, commands: [{
                command: ['test'], exitCode: 1, output: '<script>unsafe</script>',
            }] },
        }],
    } }));
    assert.match(evidenceHtml, /tokens unknown/);
    assert.match(evidenceHtml, /&lt;script&gt;unsafe&lt;\/script&gt;/);
    assert.ok(!evidenceHtml.includes('<script>unsafe'));
    console.log('Evidence UI passed: unknown usage and escaped verification output.');
    const { apiCostLabel } = await server.ssrLoadModule('/src/usage.ts');
    assert.match(apiCostLabel({ runs: 2, meteredRuns: 2, meteredRunsWithCost: 1,
        unknownBillingRuns: 0, apiCostUsd: 1 }, 1.65), /incomplete cost coverage/);
    assert.match(apiCostLabel({ runs: 1, meteredRuns: 0, subscriptionRuns: 1,
        unknownBillingRuns: 0, apiCostUsd: 0 }, 1.65), /no additional API spend/);
    const { default: RunButton } = await server.ssrLoadModule('/src/components/RunAgentButton.vue');
    const approvalHtml = await renderToString(createSSRApp(RunButton, {
        taskKey: 'LOCAL-UI', node: { state: 'READY', effectiveConfig: {
            requiresMeteredApproval: true, harness: 'metered', model: 'configured-model',
        } },
    }).use(pinia));
    assert.match(approvalHtml, /Approve API launch/);
    assert.match(approvalHtml, /disabled/);
    const { default: Budget } = await server.ssrLoadModule('/src/components/TaskBudget.vue');
    const group = { knownUsd: 8, reservedUsd: 1, limitUsd: 10, unknownRuns: 1, warning: true };
    const budgetHtml = await renderToString(createSSRApp(Budget, { task: {
        jiraKey: 'LOCAL-UI', budget: { task: group, daily: group, dayUtc: '2026-09-22' },
    } }).use(pinia));
    assert.match(budgetHtml, /Budget warning/);
    assert.match(budgetHtml, /unknown cost/);
    console.log('Cost UI passed: partial cost, subscription usage, explicit approval, budget warning.');
    const { default: Home } = await server.ssrLoadModule('/src/views/HomeView.vue');
    const { useTaskStore } = await server.ssrLoadModule('/src/stores/tasks.ts');
    const taskStore = useTaskStore(pinia);
    taskStore.tasks = [{ jiraKey: 'LOCAL-UI', jiraTitle: '<script>ticket</script>',
        status: 'ACTIVE', phase: 'Worker needs input', repositoryRoot: 'D:/Repos/project',
        createdAt: '2026-09-22T00:00:00Z', attention: [{ nodeKey: 'worker_abc', label: 'API worker',
            state: 'WAITING_FOR_USER', since: '2026-09-22T00:00:00Z', questions: 2,
            action: 'Answer questions' }],
    }];
    const { createRouter, createMemoryHistory } = await import(
        new URL('../frontend/node_modules/vue-router/dist/vue-router.mjs', import.meta.url).href
    );
    const router = createRouter({ history: createMemoryHistory(), routes: [
        { path: '/', component: Home }, { path: '/tasks/new', component: Home },
        { path: '/tasks/:jiraKey', name: 'task', component: Home },
    ] });
    await router.push('/');
    const homeHtml = await renderToString(createSSRApp(Home).use(pinia).use(router));
    assert.match(homeHtml, /Needs attention/);
    assert.match(homeHtml, /Answer questions/);
    assert.match(homeHtml, /node=worker_abc/);
    assert.match(homeHtml, /&lt;script&gt;ticket&lt;\/script&gt;/);
    assert.match(homeHtml, /Search tickets/);
    assert.ok(!homeHtml.includes('<script>ticket'));
    console.log('Dashboard rendering passed: attention queue, worker link, search, escaped ticket title.');
} finally {
    await server.close();
}
