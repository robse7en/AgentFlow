import { request } from './client';

export const instructionRoles = [
    'architect', 'orchestrator', 'implementer', 'code_reviewer', 'security_reviewer',
] as const;
export type InstructionRole = typeof instructionRoles[number];
export interface InstructionSource {
    path: string;
    content: string | null;
    hash: string | null;
}
export interface InstructionSnapshot {
    schema: number;
    version: string;
    repository: string;
    revision: string;
    freshConversations: boolean;
    files: Record<InstructionRole, InstructionSource>;
}
export interface InstructionPreview {
    repository: string;
    files: Record<InstructionRole, InstructionSource> | null;
    snapshot?: InstructionSnapshot | null;
    sourceError?: string | null;
    dirtyPaths: string[];
    differs?: boolean | null;
    briefing?: string | null;
    runs?: { id: number; nodeKey: string }[];
}
export interface HistoricalInstructions {
    runId: number;
    metadata: {
        version: string;
        repository?: string;
        revision: string;
        role: InstructionRole;
        source: InstructionSource;
        briefingHash: string;
    } | null;
    briefing: string;
}
export const instructionApi = {
    load: (base: string, node: string, preview: 'snapshot' | 'source') =>
        request<InstructionPreview>(
            `${base}/instructions?${new URLSearchParams({ nodeKey: node, preview })}`,
        ),
    history: (base: string, runId: number) =>
        request<HistoricalInstructions>(`${base}/instructions?runId=${runId}`),
    save: (base: string, role: InstructionRole, content: string, expectedHash: string | null) =>
        request<InstructionSource>(`${base}/instructions/${role}`, {
            method: 'PUT', body: JSON.stringify({ content, expectedHash }),
        }),
    apply: (base: string, preview: InstructionPreview, freshConversations: boolean) =>
        request<InstructionSnapshot>(`${base}/instructions/apply`, {
            method: 'POST',
            body: JSON.stringify({
                expectedVersion: preview.snapshot?.version,
                expectedSourceHashes: Object.fromEntries(
                    Object.entries(preview.files ?? {}).map(([role, file]) => [role, file.hash]),
                ),
                freshConversations,
            }),
        }),
};
