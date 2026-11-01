// What FindingCode.vue draws for each view of a finding's code: numbered rows, the flagged ones
// marked, and where the finding's comment goes. Kept apart from the component so it stays plain
// logic over the API's FindingCode.
import type { DiffHunk, Finding, FindingCode } from '@/api/types';

export type CodeMode = 'snapshot' | 'diff' | 'now';

/** Lines shown around the flagged ones, and how many more each expand click reveals. */
export const CONTEXT = 4;
export const EXPAND = 10;

export interface CodeRow {
  kind: 'context' | 'add' | 'del' | 'hunk';
  /** Line numbers on the left (old / only) and right (new) of the gutter. */
  old: number | null;
  new: number | null;
  /** Highlighted HTML (escaped text for hunk headers). */
  html: string;
  flagged: boolean;
  /** The finding's comment goes right after this row. */
  comment: boolean;
}

/** The flagged range, clamped to a file of `count` lines; null when there is no line. */
export function flaggedRange(line: number | null, endLine: number | null, count: number): [number, number] | null {
  if (line == null || count === 0) return null;
  const first = Math.min(Math.max(line, 1), count);
  const last = Math.min(Math.max(endLine ?? first, first), count);
  return [first, last];
}

/** The rows first..last of a whole file, with the flagged lines marked. */
export function fileRows(html: string[], window: [number, number], flagged: [number, number] | null): CodeRow[] {
  const rows: CodeRow[] = [];
  for (let n = window[0]; n <= window[1]; n++) {
    const isFlagged = !!flagged && n >= flagged[0] && n <= flagged[1];
    rows.push({ kind: 'context', old: n, new: null, html: html[n - 1] ?? '', flagged: isFlagged,
      comment: !!flagged && n === flagged[1] });
  }
  return rows;
}

/** The lines around the flagged ones, `CONTEXT` each side. */
export function initialWindow(flagged: [number, number] | null, count: number): [number, number] {
  if (!flagged) return [1, Math.min(count, 2 * CONTEXT + 1)];
  return [Math.max(1, flagged[0] - CONTEXT), Math.min(count, flagged[1] + CONTEXT)];
}

/**
 * A diff's rows. `oldHtml`/`newHtml` are the highlighted files on either side when known (a side
 * that isn't is highlighted row by row with `single`); `flaggedSide` says which side's numbers the
 * flagged range is in.
 */
export function diffRows(
  hunks: DiffHunk[], single: (text: string) => string, oldHtml: string[] | null, newHtml: string[] | null,
  flagged: [number, number] | null, flaggedSide: 'old' | 'new',
): CodeRow[] {
  const rows: CodeRow[] = [];
  for (const hunk of hunks) {
    rows.push({ kind: 'hunk', old: null, new: null, html: escapeText(hunk.header), flagged: false, comment: false });
    for (const row of hunk.rows) {
      const html = row.new != null && newHtml ? newHtml[row.new - 1] : row.old != null && oldHtml ? oldHtml[row.old - 1] : null;
      const number = flaggedSide === 'old' ? row.old : row.new;
      const isFlagged = !!flagged && number != null && number >= flagged[0] && number <= flagged[1];
      rows.push({ kind: row.kind, old: row.old, new: row.new, html: html ?? single(row.text), flagged: isFlagged, comment: false });
    }
  }
  // The comment follows the last flagged row; a deleted range's comment follows its replacement.
  let last = -1;
  rows.forEach((row, i) => { if (row.flagged) last = i; });
  if (last >= 0 && rows[last].kind === 'del') {
    while (last + 1 < rows.length && (rows[last + 1].kind === 'del' || rows[last + 1].kind === 'add')) last++;
  }
  if (last >= 0) rows[last].comment = true;
  return rows;
}

function escapeText(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/** The hunks holding any flagged line (on `side`); all of them when none does. */
export function flaggedHunks(hunks: DiffHunk[], flagged: [number, number] | null, side: 'old' | 'new'): DiffHunk[] {
  if (!flagged) return hunks;
  const hit = hunks.filter((hunk) => hunk.rows.some((row) => {
    const n = row[side];
    return n != null && n >= flagged[0] && n <= flagged[1];
  }));
  return hit.length ? hit : hunks;
}

/** A path's file name: the header shows that, the full path is in its tooltip. */
export function fileName(path: string): string {
  return path.split('/').pop() || path;
}

/** Which diff the Diff view shows: what changed since the review for a closed finding (when
 *  anything did), else the task's change up to the reviewed commit. */
export function diffSource(code: FindingCode, finding: Finding): 'since' | 'task' | null {
  const since = code.now && code.now.sha !== code.reviewedSha && code.now.hunks.length ? 'since' : null;
  if (finding.status !== 'open' && since) return since;
  if (code.diff) return 'task';
  return since;
}

export function shortSha(sha: string | null | undefined): string {
  return sha ? sha.slice(0, 7) : '';
}

/** `file:line` or `file:line-end`, as copied to the clipboard and shown in the header. */
export function location(file: string, line: number | null, endLine: number | null): string {
  if (line == null) return file;
  return endLine != null && endLine > line ? `${file}:${line}-${endLine}` : `${file}:${line}`;
}
