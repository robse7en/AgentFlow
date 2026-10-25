// Local SSR rendering checks for the actual editor. No browser/provider acceptance is implied.
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
const originalFetch = globalThis.fetch;
// Existing Modal's overlay stack touches the body style even under SSR.
globalThis.document = { body: { style: {} } };
try {
    const { default: Instructions } = await server.ssrLoadModule('/src/components/RepositoryInstructions.vue');
    const roles = ['architect', 'orchestrator', 'implementer', 'code_reviewer', 'security_reviewer'];
    const files = Object.fromEntries(roles.map((role) => [role, {
        path: `.agentflow-instructions/${role.replaceAll('_', '-')}.md`,
        content: `<script>alert('source')</script> ${role}`, hash: `hash-${role}`,
    }]));
    const snapshot = {
        version: 'immutable-version-1', repository: 'target/worktree', revision: 'abc123',
        files: structuredClone(files), freshConversations: false,
    };
    snapshot.files.architect.content = 'Frozen Architect guidance';
    let dirtyPaths = [' M .agentflow-instructions/architect.md'];
    globalThis.fetch = async () => new Response(JSON.stringify({
        repository: 'target/worktree', files, snapshot, differs: true,
        dirtyPaths,
        briefing: '<img src=x onerror=alert(1)> Effective frozen briefing',
        runs: [{ id: 123, nodeKey: 'architect' }],
    }), { status: 200, headers: { 'Content-Type': 'application/json' } });
    const render = async (props) => {
        const context = {};
        const html = await renderToString(createSSRApp(Instructions, props), context);
        return html + (context.teleports?.body ?? '');
    };
    const task = { jiraKey: 'LOCAL-TEST', nodes: [{ key: 'architect', role: 'architect', label: 'Discovery' }] };
    const effective = await render({ task });
    for (const text of ['immutable-version-1', 'target/worktree', 'abc123',
        'Saved sources differ', 'Historical run (read only)', 'Frozen Architect guidance',
        'Effective future briefing', 'Resumed conversations can retain older instructions']) {
        assert.ok(effective.includes(text), text);
    }
    assert.ok(effective.includes('&lt;img src=x onerror=alert(1)&gt;'));
    assert.ok(!effective.includes('<img src=x'));
    const editor = await render({ project: { id: 1 } });
    assert.ok(editor.includes('&lt;script&gt;'));
    assert.ok(!editor.includes('<script>'));
    assert.ok(editor.includes('Saved instruction edits are uncommitted'));
    assert.ok(editor.includes('Save source'));
    assert.ok(editor.includes('Git worktree creation carries committed files'));
    for (const entry of [
        'M  .agentflow-instructions/architect.md',
        'AM .agentflow-instructions/architect.md',
        '?? .agentflow-instructions/',
        ' D .agentflow-instructions/architect.md',
        'R  old.md -> .agentflow-instructions/architect.md',
        ' R .agentflow-instructions/architect.md -> moved.md',
        'R  "old name.md" -> ".agentflow-instructions/architect.md"',
    ]) {
        dirtyPaths = [entry];
        assert.ok((await render({ project: { id: 1 } }))
            .includes('Saved instruction edits are uncommitted'), entry);
    }
    dirtyPaths = [' M unrelated.md', '?? .agentflow-instructions-other/architect.md'];
    assert.ok((await render({ project: { id: 1 } }))
        .includes('No uncommitted instruction-file changes'));
    const { default: History } = await server.ssrLoadModule('/src/components/InstructionRunHistory.vue');
    const history = await renderToString(createSSRApp(History, { history: {
        runId: 123, metadata: { version: 'historical-version-0', repository: 'old/worktree',
            revision: 'old-revision', role: 'implementer', source: files.implementer,
            briefingHash: 'exact-old-hash' }, briefing: '<script>Old briefing</script>',
    } }));
    for (const text of ['historical-version-0', 'old/worktree', 'old-revision',
        'implementer.md', 'exact-old-hash', '&lt;script&gt;Old briefing&lt;/script&gt;']) {
        assert.ok(history.includes(text), text);
    }
    assert.ok(!history.includes('immutable-version-1'));
    assert.ok(!history.includes('<script>'));
    console.log('Instruction UI SSR checks passed: source/snapshot distinction, historical version, Git status, text escaping.');
} finally {
    globalThis.fetch = originalFetch;
    await server.close();
}
