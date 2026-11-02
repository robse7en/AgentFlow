// Local rendering check of the task header's pull-request step; makes no network requests.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(new URL('../frontend/package.json', import.meta.url));
const { createServer } = require('vite');
const root = fileURLToPath(new URL('../frontend', import.meta.url));
const server = await createServer({ root, server: { middlewareMode: true }, appType: 'custom' });

try {
    const { default: PrHeaderAction } = await server.ssrLoadModule('/src/components/PrHeaderAction.vue');
    const { createSSRApp } = require('vue');
    const { renderToString } = require('@vue/server-renderer');
    const { createPinia, setActivePinia } = await import(
        new URL('../frontend/node_modules/pinia/dist/pinia.mjs', import.meta.url).href
    );
    const render = async (task) => {
        const app = createSSRApp(PrHeaderAction, { task });
        const pinia = createPinia();
        setActivePinia(pinia);
        app.use(pinia);
        return renderToString(app);
    };
    const base = { jiraKey: 'LOCAL-UI', branchName: 'agent/LOCAL-UI', baseBranch: 'main', prUrl: null };

    const ready = await render({ ...base, status: 'READY_FOR_PR' });
    assert.match(ready, /class="primary pr-cta"/);
    assert.match(ready, /Push &amp; open PR/);
    assert.match(ready, /→<\/span> main/);
    assert.doesNotMatch(ready, /unusual/);

    // A base other than main/master is flagged, since GitHub defaults elsewhere.
    const release = await render({ ...base, status: 'READY_FOR_PR', baseBranch: 'release/candidate' });
    assert.match(release, /class="(unusual into|into unusual)"/);
    assert.match(release, /release\/candidate/);

    const created = await render({ ...base, status: 'PR_CREATED', prUrl: 'https://github.com/acme/repo/pull/22832' });
    assert.match(created, /PR #22832 ↗/);
    assert.match(created, /Mark merged/);
    assert.doesNotMatch(created, /Push &amp; open PR/);

    // Nothing to do in the header before Ready for PR or after Merged.
    for (const status of ['ACTIVE', 'NEEDS_HUMAN_REVIEW', 'MERGED', 'CANCELLED']) {
        const html = await render({ ...base, status, prUrl: status === 'MERGED' ? 'https://github.com/a/b/pull/1' : null });
        assert.doesNotMatch(html, /<button|<a /, status);
    }
    console.log('PR header action renders each status');
} finally {
    await server.close();
}
