// How the Findings tab sorts, filters and groups review findings. Every review round stores a row
// per finding; a re-raised finding keeps its id (its earlier rows end up superseded), so the list
// shows each id once, as its newest row, and the detail shows the earlier rows as its history.
import type { Finding } from '@/api/types';

export type StatusFilter = 'open' | 'blocking' | 'all';
export type TypeFilter = 'all' | 'code-review' | 'security-review';
export interface FindingFilter { status: StatusFilter; type: TypeFilter }

export const TYPE_LABELS: Record<string, string> = { 'code-review': 'Code review', 'security-review': 'Security review' };
export const typeLabel = (type: string): string => TYPE_LABELS[type] ?? type;

const SEVERITY_RANK: Record<string, number> = { critical: 0, high: 1, medium: 2, low: 3, info: 4 };

/** Open and blocking: what stops the task moving on. A closed finding no longer blocks. */
export const blocksNow = (f: Finding): boolean => f.status === 'open' && f.blocking;

/** Findings whose code can be drawn: they name a file and a line the reviewer looked at. */
export const hasCode = (f: Finding): boolean => !!f.file && f.line != null && !!f.reviewedSha;

const newer = (a: Finding, b: Finding): boolean => a.iteration > b.iteration || (a.iteration === b.iteration && a.dbId > b.dbId);

/** Each finding id's newest row. */
export function latestRows(findings: Finding[]): Finding[] {
  const byId = new Map<string, Finding>();
  for (const f of findings) {
    const current = byId.get(f.id);
    if (!current || newer(f, current)) byId.set(f.id, f);
  }
  return [...byId.values()];
}

/** The rows before `latest` with its id, oldest first. */
export function earlierRows(findings: Finding[], latest: Finding): Finding[] {
  return findings
    .filter((f) => f.id === latest.id && f.dbId !== latest.dbId)
    .sort((a, b) => a.iteration - b.iteration || a.dbId - b.dbId);
}

/** Blocking first, then the rest of the open ones, then closed; by severity, then id, within each. */
export function compareFindings(a: Finding, b: Finding): number {
  const rank = (f: Finding) => (blocksNow(f) ? 0 : f.status === 'open' ? 1 : 2);
  return rank(a) - rank(b)
    || (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9)
    || a.id.localeCompare(b.id, undefined, { numeric: true });
}

export function matchesStatus(f: Finding, status: StatusFilter): boolean {
  if (status === 'open') return f.status === 'open';
  if (status === 'blocking') return blocksNow(f);
  return true;
}

export function matchesType(f: Finding, type: TypeFilter): boolean {
  return type === 'all' || f.type === type;
}

/** Rows by the iteration that last raised them, newest first, each sorted by `compareFindings`. */
export function groupByIteration(rows: Finding[]): [number, Finding[]][] {
  const map = new Map<number, Finding[]>();
  for (const f of rows) map.set(f.iteration, [...(map.get(f.iteration) ?? []), f]);
  return [...map.entries()].sort((a, b) => b[0] - a[0]).map(([iteration, items]) => [iteration, items.sort(compareFindings)]);
}

/** The tab opens on what still needs doing when anything does, else on everything. */
export function defaultFilter(findings: Finding[]): FindingFilter {
  return { status: latestRows(findings).some((f) => f.status === 'open') ? 'open' : 'all', type: 'all' };
}

/** The findings a reviewer node raised, by its key (the backend names a finding's type after it). */
export function nodeFindingType(nodeKey: string): TypeFilter {
  if (nodeKey === 'code_review') return 'code-review';
  if (nodeKey === 'security_review') return 'security-review';
  return 'all';
}

/** A row's status as the list shows it: an open finding the implementer answered says how. */
export function statusLabel(f: Finding): string {
  return f.status === 'open' && f.response ? `open · ${f.response}` : f.status;
}
