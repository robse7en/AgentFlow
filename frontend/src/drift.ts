// Shared "worktree drifted after review" handling for the approval node and the task view's
// review-override panel: both offer to discard uncommitted drift, with the same confirm step.
import { ref, type Ref } from 'vue';
import { ApiError } from '@/api/client';
import { useTaskStore } from '@/stores/tasks';

export const WORKTREE_DRIFT_CODE = 'worktree_drift';
export const DRIFT_DISCARDED_NOTICE =
    'Drift discarded; the worktree matches the reviewed checkpoint again.';

export const isDriftError = (error: unknown): boolean =>
    error instanceof ApiError && error.code === WORKTREE_DRIFT_CODE;

export function useWorktreeDrift(taskKey: () => string, busy: Ref<boolean>, message: Ref<string | null>) {
    const store = useTaskStore();
    // Sticky rather than derived from the latest error: a failed "Discard drift" attempt (e.g. it
    // refuses because there are real new commits) surfaces a different error than the original
    // drift one, and should not make the discard option disappear.
    const worktreeDrift = ref(false);
    const confirmingDiscard = ref(false);
    const discarded = ref(false);

    /** Record a failed action's error; keeps the discard controls visible when it was drift. */
    const noteFailure = (error: unknown): void => {
        message.value = (error as Error).message;
        if (isDriftError(error)) worktreeDrift.value = true;
    };

    /** Call before starting an action so stale drift/discard state doesn't linger. */
    const beginAction = (): void => {
        message.value = null;
        discarded.value = false;
        confirmingDiscard.value = false;
    };

    const clearDrift = (): void => { worktreeDrift.value = false; };

    const discardDrift = async (): Promise<void> => {
        busy.value = true;
        try {
            await store.discardWorktreeDrift(taskKey());
            message.value = null;
            discarded.value = true;
            worktreeDrift.value = false;
        } catch (error) {
            message.value = (error as Error).message;
        } finally {
            busy.value = false;
            confirmingDiscard.value = false;
        }
    };

    return { worktreeDrift, confirmingDiscard, discarded, noteFailure, beginAction, clearDrift, discardDrift };
}
