// Decision-tree questions in the UI: the card's logic (src/decisions.ts) and its rendering.
// Local only; uses installed frontend dependencies and makes no network requests.
//     node scripts/verify-decision-ui.mjs
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(new URL('../frontend/package.json', import.meta.url));
const { createServer } = require('vite');
const root = fileURLToPath(new URL('../frontend', import.meta.url));
// No dependency pre-bundling: SSR loads modules directly, and a scan would outlive the server.
const server = await createServer({
    root, server: { middlewareMode: true }, appType: 'custom', logLevel: 'error', optimizeDeps: { noDiscovery: true, include: [] },
});

const tree = {
    context: '4,108 accounts inactive', risk: 'high',
    choices: [
        { label: 'Soft-delete, keep invoices', recommended: true, followUp: { message: 'When are they purged?', choices: [
            { label: 'After their invoices close', recommended: true, detail: 'A nightly job purges' },
            { label: 'Never', warning: 'Table grows', note: 'Legal asked for a purge date.', source: 'docs/privacy.md' },
        ] } },
        { label: 'Skip accounts with open invoices', followUp: { message: 'Tell them?', choices: [
            { label: 'Email them' }, { label: 'No email' },
        ] } },
        { label: 'Hard-delete <everything>', warning: 'Breaks history', detail: 'Needs a migration',
          note: 'Billing reads owner_id.', source: 'https://example.com/billing' },
    ],
};
const question = (fields = {}) => ({
    uid: 'q-1', role: 'architect', nodeKey: 'architect', message: 'What should the cleanup do?',
    choices: tree.choices.map((c) => c.label), decision: tree, answered: false, answer: null,
    createdAt: '2026-10-25T10:00:00Z', answeredAt: null, runId: 1, actionable: true, ...fields,
});

try {
    const d = await server.ssrLoadModule('/src/decisions.ts');

    // What a question offers.
    const view = d.decisionView(question());
    assert.equal(view.tree, true);
    assert.equal(view.pick, 0);
    assert.equal(view.otherLimit, 3993, 'the server adds "other: " to free text on a tree');
    const flat = d.decisionView(question({ decision: null, choices: ['Gradual', 'All at once'] }));
    assert.deepEqual([flat.tree, flat.pick, flat.risk, flat.otherLimit, flat.branches.map((b) => b.label)],
        [false, null, null, 4000, ['Gradual', 'All at once']]);

    // States: the agent's pick is highlighted by default, hover previews another branch, picking a
    // branch fades the others out in place, a sent choice keeps only its path.
    const state = (fields) => ({ ...d.initialState(), phase: 'idle', ...fields });
    const branches = (s, v = view) => v.branches.map((_, i) => d.branchState(v, s, i));
    assert.deepEqual(branches(state()), ['on', 'dim', 'dim']);
    assert.deepEqual(branches(state(), flat), ['', '']);
    assert.deepEqual(branches(state({ hover: 2 })), ['dim', 'dim', 'on']);
    assert.deepEqual(branches(state({ picked: 1 })), ['gone', 'on', 'gone']);
    assert.deepEqual(branches(state({ otherOpen: true })), ['dim', 'dim', 'dim']);
    assert.deepEqual(branches(state({ chosen: { branch: 0, option: 1, other: null } })), ['on', 'gone', 'gone']);
    assert.deepEqual(branches(state({ chosen: { branch: null, option: null, other: 'text' } })), ['gone', 'gone', 'gone']);
    assert.equal(d.optionState(view, state({ picked: 0, hoverOption: { branch: 0, option: 1 } }), 0, 1), 'lift');
    assert.equal(d.optionState(view, state({ picked: 0, hoverOption: { branch: 0, option: 1 } }), 0, 0), 'dim');
    assert.equal(d.optionState(view, state({ picked: 0 }), 1, 0), 'gone');
    assert.equal(d.optionState(view, state({ chosen: { branch: 0, option: 1, other: null } }), 0, 1), 'on');
    assert.deepEqual([d.otherState(view, state()), d.otherState(flat, state()), d.otherState(view, state({ otherOpen: true })),
        d.otherState(view, state({ picked: 0 }))], ['dim', '', 'on', 'gone']);
    assert.deepEqual(d.answerPath({ branch: 0, option: 1, other: null }), [0, 1]);
    assert.deepEqual(d.answerPath({ branch: 2, option: null, other: null }), [2]);
    assert.equal(d.answerPath({ branch: null, option: null, other: 'x' }), null);

    // Keys: 1–N branches, N+1 or O Other…, then 1–M options, Esc back, → next (inbox only), and
    // nothing while sending or entering.
    const key = (k, s = state(), v = view, next = false) => d.keyAction(k, v, s, next);
    assert.deepEqual(key('1'), { kind: 'branch', index: 0 });
    assert.deepEqual(key('3'), { kind: 'branch', index: 2 });
    assert.deepEqual(key('4'), { kind: 'other' });
    assert.equal(key('5'), null);
    assert.deepEqual(key('o'), { kind: 'other' });
    assert.deepEqual(key('2', state({ picked: 0 })), { kind: 'option', index: 1 });
    assert.equal(key('3', state({ picked: 0 })), null);
    assert.equal(key('o', state({ picked: 0 })), null);
    assert.deepEqual(key('Escape', state({ picked: 0 })), { kind: 'back' });
    assert.equal(key('Escape'), null);
    assert.equal(key('ArrowRight'), null);
    assert.deepEqual(key('ArrowRight', state(), view, true), { kind: 'next' });
    assert.equal(key('1', state({ phase: 'sending' })), null);
    assert.equal(key('1', state({ phase: 'entering' })), null);
    assert.equal(key('1', state({ chosen: { branch: 2, option: null, other: null } })), null);

    // Keys count only on the page itself or inside the card: never in the terminal drawer, a text
    // field, or with a modifier.
    const origin = (fields) => ({ onBody: false, inCard: false, inTerminal: false, editable: false, modifier: false, ...fields });
    assert.equal(d.keyAllowed(origin({ onBody: true })), true);
    assert.equal(d.keyAllowed(origin({ inCard: true })), true);
    assert.equal(d.keyAllowed(origin({})), false, 'focus elsewhere on the page');
    assert.equal(d.keyAllowed(origin({ inTerminal: true, editable: true })), false);
    assert.equal(d.keyAllowed(origin({ inTerminal: true, inCard: false })), false);
    assert.equal(d.keyAllowed(origin({ inCard: true, editable: true })), false, 'typing in Other…');
    assert.equal(d.keyAllowed(origin({ onBody: true, modifier: true })), false);

    assert.deepEqual(d.hint(view, state()), { kind: 'pick', last: 4 });
    assert.deepEqual(d.hint(view, state({ picked: 1 })), { kind: 'then', last: 2 });
    assert.deepEqual(d.hint(view, state({ otherOpen: true })), { kind: 'other' });
    assert.deepEqual(d.hint(view, state({ phase: 'sent', chosen: { branch: 2, option: null, other: null } })), { kind: 'none' });

    // After the answer: "Sent to {node}", the node's real state, and the line typed.
    const snapshot = (nodeState, open) => ({
        nodes: [{ key: 'architect', state: nodeState }],
        questions: [{ ...question(), answered: true, answer: '1.2: Soft-delete, keep invoices → Never', actionable: false },
            ...Array.from({ length: open }, (_, i) => ({ ...question({ uid: `q-${i + 2}` }) }))],
    });
    assert.deepEqual(d.sentStatus(snapshot('RUNNING', 0), question(), 'Architect'),
        { title: 'Sent to Architect · running again', state: 'RUNNING', line: '1.2: Soft-delete, keep invoices → Never', remaining: 0 });
    assert.equal(d.sentStatus(snapshot('WAITING_FOR_USER', 1), question(), 'Architect').title,
        'Sent to Architect · waiting for 1 more answer');
    assert.equal(d.sentStatus(snapshot('WAITING_FOR_USER', 2), question(), 'Architect').title,
        'Sent to Architect · waiting for 2 more answers');

    // The inbox is oldest first across tasks.
    const open = [
        { ...question({ uid: 'b' }), jiraKey: 'PROJ-2', createdAt: '2026-10-25T10:05:00Z' },
        { ...question({ uid: 'a' }), jiraKey: 'PROJ-9', createdAt: '2026-10-25T09:00:00Z' },
        { ...question({ uid: 'c' }), jiraKey: 'PROJ-1', createdAt: '2026-10-25T10:05:00Z' },
    ];
    assert.deepEqual(d.inboxOrder(open).map((q) => q.uid), ['a', 'c', 'b']);
    assert.deepEqual(open.map((q) => q.uid), ['b', 'a', 'c'], 'sorting does not reorder the store\'s list');

    assert.equal(d.sourceHref('https://example.com/x'), 'https://example.com/x');
    assert.equal(d.sourceHref('docs/privacy.md'), null);
    assert.equal(d.sourceHref('javascript:alert(1)'), null);
    const now = Date.parse('2026-10-25T10:00:00Z');
    assert.deepEqual(['2026-10-25T09:59:30Z', '2026-10-25T09:56:00Z', '2026-10-25T07:00:00Z', '2026-10-22T10:00:00Z']
        .map((t) => d.ageLabel(t, now)), ['just now', '4m ago', '3h ago', '3d ago']);
    assert.ok(d.elbow(0, 10, 20, 40, 50).d.startsWith('M 0 10 H 10 Q 20 10 20 20'));
    assert.equal(d.elbow(0, 10, 20, 40, 10).d, 'M 0 10 H 40');
    console.log('Decision logic passed: views, states, keys and their guard, hints, sent status, inbox order.');

    // Rendering, before any interaction: every branch, the pick, consequences, notes, Other….
    const { createSSRApp } = require('vue');
    const { renderToString } = require('@vue/server-renderer');
    const { default: Card } = await server.ssrLoadModule('/src/components/DecisionCard.vue');
    const render = (q, extra = {}) => renderToString(createSSRApp(Card, {
        question: q, nodeLabel: 'Architect', meta: 'Claude Code · opus · 4m ago', submit: async () => ({}), ...extra,
    }));
    const html = await render(question());
    for (const text of ['High risk', 'What should the cleanup do?', '4,108 accounts inactive', 'Soft-delete, keep invoices',
        'When are they purged?', 'After their invoices close', 'Breaks history', 'Needs a migration', 'Other…',
        'Claude Code · opus · 4m ago', 'Pick a branch']) {
        assert.ok(html.includes(text), `renders ${text}`);
    }
    assert.equal((html.match(/Agent&#39;s pick|Agent's pick/g) ?? []).length, 2, 'the pick at both levels');
    assert.equal((html.match(/Has an agent&#39;s note|Has an agent's note/g) ?? []).length, 2, 'note markers');
    assert.ok(html.includes('Hard-delete &lt;everything&gt;') && !html.includes('<everything>'), 'agent text is escaped');
    assert.match(html, /class="dc-kbd"[^>]*>4<\/span><span[^>]*>Other…/, 'Other… is key 4');
    const flatHtml = await render(question({ decision: null, choices: ['Gradual', 'All at once'] }));
    assert.ok(flatHtml.includes('Gradual') && !flatHtml.includes('risk') && !/Agent(&#39;|')s pick/.test(flatHtml));
    const freeHtml = await render(question({ decision: null, choices: [] }));
    assert.ok(freeHtml.includes('Other…'), 'a free-text question starts with Other… (opened on mount)');
    console.log('Decision card rendering passed: tree, flat and free-text questions, escaped agent text.');
} finally {
    await server.close();
}
