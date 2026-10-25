// Exercise the actual Vue component with server time independent of this machine's clock.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(new URL('../frontend/package.json', import.meta.url));
const { createServer } = require('vite');
const { createSSRApp } = require('vue');
const { renderToString } = require('@vue/server-renderer');
const root = fileURLToPath(new URL('../frontend', import.meta.url));
const server = await createServer({
    root, server: { middlewareMode: true }, appType: 'custom',
    optimizeDeps: { noDiscovery: true, entries: [] },
});
try {
    const { default: Heartbeat } = await server.ssrLoadModule('/src/components/AgentHeartbeat.vue');
    const snapshot = (age, state = 'RUNNING', received = true) => ({
        state,
        lastRun: {
            state,
            startedAt: '2020-01-01T00:00:00Z',
            heartbeat: {
                observedAt: `2020-01-01T00:${age}:00Z`,
                warningSince: '2020-01-01T00:00:00Z',
                receivedAt: received ? '2020-01-01T00:00:00Z' : null,
                phase: 'testing',
                warningSeconds: 600,
            },
        },
    });
    const render = (node) => renderToString(createSSRApp(Heartbeat, { node }));
    assert.match(await render(snapshot('09')), /Agent reported.*9m ago/);
    assert.match(await render(snapshot('10')), /Signal overdue/);
    assert.match(await render(snapshot('10', 'RUNNING', false)), /Signal overdue.*started 10m ago/);
    assert.match(await render(snapshot('02', 'RUNNING', false)), /Awaiting first agent signal/);
    assert.match(await render(snapshot('30', 'WAITING_FOR_USER')), /warnings paused/);
    assert.doesNotMatch(await render(snapshot('30', 'INTERRUPTED')), /Signal overdue/);
    assert.doesNotMatch(await render(snapshot('30', 'COMPLETED')), /Signal overdue/);
    assert.doesNotMatch(await render({ state: 'RUNNING' }), /Agent reported|Signal overdue/);
    const resumed = snapshot('30');
    resumed.lastRun.heartbeat.warningSince = '2020-01-01T00:29:00Z';
    assert.doesNotMatch(await render(resumed), /Signal overdue/);
    const unsafe = snapshot('01');
    unsafe.lastRun.heartbeat.phase = '<script>alert(1)</script>';
    assert.match(await render(unsafe), /&lt;script&gt;/);
    console.log('Heartbeat UI checks passed: threshold, no-signal, waits, recovery grace, clock skew, escaping.');
} finally {
    await server.close();
}
