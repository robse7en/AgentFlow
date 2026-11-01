<script lang="ts" setup>
// The code a review finding points at, drawn like a GitHub review comment: the flagged lines
// highlighted in their file, with the finding's text (the default slot) right below them.
import { computed, onMounted, ref, watch } from 'vue';
import { api } from '@/api/client';
import type { Finding, FindingCode } from '@/api/types';
import { highlightLines } from '@/highlight';
import {
  type CodeMode, type CodeRow, EXPAND, diffRows, diffSource, fileName, fileRows, flaggedHunks, flaggedRange, initialWindow,
  location, shortSha,
} from '@/findingCode';

const props = defineProps<{ taskKey: string; finding: Finding; currentSha: string | null }>();

const code = ref<FindingCode | null>(null);
const failed = ref<string | null>(null);
const mode = ref<CodeMode>(props.finding.status === 'open' ? 'snapshot' : 'diff');
/** The visible lines of the Snapshot and Now views, widened by the expand rows. */
const windows = ref<Partial<Record<'snapshot' | 'now', [number, number]>>>({});
const copied = ref(false);

/** Only the latest request may land: an older answer arriving late would show stale code. */
let latest = 0;
async function load() {
  const request = ++latest;
  failed.value = null;
  try {
    const data = await api.findingCode(props.taskKey, props.finding.dbId);
    if (request !== latest) return;
    code.value = data;
    // The Now view's lines moved with the new commit; the snapshot's did not.
    windows.value = { snapshot: windows.value.snapshot };
  } catch (error) {
    if (request === latest) failed.value = error instanceof Error ? error.message : String(error);
  }
}
onMounted(load);
// A new checkpoint changes the Now view and the since-review diff.
watch(() => props.currentSha, load);

const path = computed(() => code.value?.file?.replace(/\\/g, '/').replace(/^(\.\/)+/, '') ?? props.finding.file ?? '');
const snapshotHtml = computed(() => {
  const snapshot = code.value?.snapshot;
  return snapshot ? highlightLines(snapshot.lines, path.value, `${snapshot.sha}:${path.value}`) : null;
});
const nowHtml = computed(() => {
  const now = code.value?.now;
  if (!now) return null;
  return now.sha === code.value?.reviewedSha
    ? snapshotHtml.value
    : highlightLines(now.lines, path.value, `${now.sha}:${path.value}`);
});

const available = computed<Record<CodeMode, boolean>>(() => ({
  snapshot: !!code.value?.snapshot,
  diff: !!code.value && !!diffSource(code.value, props.finding),
  now: !!code.value?.now,
}));
/** The view drawn: the chosen one, or the first that has something when it has nothing. */
const shown = computed<CodeMode | null>(() => {
  if (available.value[mode.value]) return mode.value;
  return (['snapshot', 'now', 'diff'] as CodeMode[]).find((m) => available.value[m]) ?? null;
});

function fileView(which: 'snapshot' | 'now') {
  const data = code.value!;
  const html = which === 'snapshot' ? snapshotHtml.value! : nowHtml.value!;
  const flagged = which === 'snapshot'
    ? flaggedRange(data.line, data.endLine, html.length)
    : flaggedRange(data.now!.line, data.now!.endLine, html.length);
  const window = windows.value[which] ?? initialWindow(flagged, html.length);
  return { rows: fileRows(html, window, flagged), window, count: html.length };
}

const single = (text: string) => highlightLines([text], path.value)[0];
/** The Diff view shows only the hunks with flagged lines until asked for the rest. */
const allHunks = ref(false);

const view = computed(() => {
  const data = code.value;
  if (!data || !shown.value) return null;
  if (shown.value !== 'diff') return { mode: shown.value, ...fileView(shown.value), hidden: 0 };
  const source = diffSource(data, props.finding)!;
  const flagged = flaggedRange(data.line, data.endLine, data.snapshot?.lines.length ?? 0);
  const [hunks, side] = source === 'since' ? [data.now!.hunks, 'old' as const] : [data.diff!.hunks, 'new' as const];
  const visible = allHunks.value ? hunks : flaggedHunks(hunks, flagged, side);
  const rows: CodeRow[] = source === 'since'
    ? diffRows(visible, single, snapshotHtml.value, nowHtml.value, flagged, side)
    : diffRows(visible, single, null, snapshotHtml.value, flagged, side);
  const [from, to] = source === 'since' ? [data.reviewedSha, data.now!.sha] : [data.diff!.from, data.diff!.to];
  return { mode: 'diff' as const, rows, source, from, to, window: null, count: 0, hidden: hunks.length - visible.length };
});

const commentShown = computed(() => !!view.value?.rows.some((row) => row.comment));

function expand(direction: 'up' | 'down') {
  const current = view.value;
  if (!current || current.mode === 'diff' || !current.window) return;
  const [first, last] = current.window;
  windows.value = {
    ...windows.value,
    [current.mode]: direction === 'up'
      ? [Math.max(1, first - EXPAND), last]
      : [first, Math.min(current.count, last + EXPAND)],
  };
}

const where = computed(() => location(path.value, props.finding.line, props.finding.endLine));
/** The side panel is narrow: the header names the file, its tooltip (and the copy) has the path. */
const shortWhere = computed(() => location(fileName(path.value), props.finding.line, props.finding.endLine));
async function copy() {
  await navigator.clipboard.writeText(where.value);
  copied.value = true;
  setTimeout(() => (copied.value = false), 1500);
}

const nowNote = computed(() => {
  const data = code.value;
  if (!data?.now) return null;
  if (data.now.sha === data.reviewedSha) return 'No commits since the review';
  const since = `since ${shortSha(data.reviewedSha)} · now ${shortSha(data.now.sha)}`;
  return data.now.changed ? `Flagged lines changed ${since}` : `Flagged lines unchanged ${since}`;
});
const diffNote = computed(() => {
  const current = view.value;
  if (!current || current.mode !== 'diff') return null;
  const label = current.source === 'since' ? 'Changed since the review' : "The task's change";
  const note = `${label} · ${shortSha(current.from)} → ${shortSha(current.to)}`;
  if (!current.rows.length) return `${note} · this file is unchanged`;
  return commentShown.value ? note : `${note} · the flagged lines aren't in it`;
});
const titles: Record<CodeMode, string> = {
  snapshot: 'The file as the reviewer saw it',
  diff: 'What changed in this file',
  now: "The same place in the task's latest commit",
};
const reasonFor = (m: CodeMode) => code.value?.unavailable[m] ?? titles[m];
</script>

<template>
  <div class="code-view">
    <div class="code-head">
      <span class="path" :title="where">{{ shortWhere }}</span>
      <span v-if="code?.reviewedSha" class="sha">@ {{ shortSha(code.reviewedSha) }}</span>
      <span class="spacer" />
      <span v-if="code" class="segmented" role="group" aria-label="Code view">
        <button
          v-for="m in (['snapshot', 'diff', 'now'] as CodeMode[])"
          :key="m"
          type="button"
          :class="{ on: shown === m }"
          :disabled="!available[m]"
          :title="reasonFor(m)"
          @click="mode = m"
        >{{ m === 'snapshot' ? 'Snapshot' : m === 'diff' ? 'Diff' : 'Now' }}</button>
      </span>
      <button type="button" class="icon-btn" :title="copied ? 'Copied' : `Copy ${where}`" @click="copy">
        {{ copied ? '✓' : '⧉' }}
      </button>
    </div>

    <p v-if="failed" class="code-note">Couldn't load the code: {{ failed }} <a href="#" @click.prevent="load">Retry</a></p>
    <p v-else-if="!code" class="code-note">Loading code…</p>
    <p v-else-if="!view" class="code-note">{{ code.unavailable.snapshot ?? 'No code to show' }}</p>
    <template v-else>
      <p v-if="view.mode === 'now' && nowNote" class="code-note" :class="{ changed: code.now?.changed }">{{ nowNote }}</p>
      <p v-if="view.mode === 'diff' && diffNote" class="code-note">{{ diffNote }}</p>
      <div class="code-scroll">
        <table class="code-lines" :class="{ diff: view.mode === 'diff' }">
          <tbody>
            <tr v-if="view.window && view.window[0] > 1" class="expand">
              <td :colspan="3"><button type="button" @click="expand('up')">↑ Show more above</button></td>
            </tr>
            <template v-for="(row, i) in view.rows" :key="i">
              <tr v-if="row.kind === 'hunk'" class="hunk"><td colspan="4" v-html="row.html" /></tr>
              <tr v-else :class="[row.kind, { flagged: row.flagged }]">
                <td class="ln">{{ row.old ?? '' }}</td>
                <td v-if="view.mode === 'diff'" class="ln">{{ row.new ?? '' }}</td>
                <td class="mark">{{ row.kind === 'add' ? '+' : row.kind === 'del' ? '−' : row.flagged ? '▍' : '' }}</td>
                <td class="src" v-html="row.html" />
              </tr>
              <tr v-if="row.comment" class="comment">
                <td :colspan="view.mode === 'diff' ? 4 : 3"><div class="bubble" :class="finding.severity"><slot /></div></td>
              </tr>
            </template>
            <tr v-if="view.window && view.window[1] < view.count" class="expand">
              <td :colspan="3"><button type="button" @click="expand('down')">↓ Show more below</button></td>
            </tr>
            <tr v-if="view.hidden" class="expand">
              <td colspan="4">
                <button type="button" @click="allHunks = true">
                  Show the other {{ view.hidden }} change{{ view.hidden === 1 ? '' : 's' }} in this file
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
    <!-- The finding's text always shows: under the code when there is no flagged row to hang it on. -->
    <div v-if="!commentShown" class="bubble loose" :class="finding.severity"><slot /></div>
  </div>
</template>
