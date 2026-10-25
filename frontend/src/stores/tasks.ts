import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { ApiError, api, connectEvents } from '@/api/client'
import { useOperationStore } from './operations'
import { useProjectStore } from './projects'
import type {
  AgentOverride, AnswerBody, CreateTaskPayload, DeleteTaskOptions, MeteredApproval, TaskListFilter, TaskSnapshot, TaskSummary,
} from '@/api/types'

export const useTaskStore = defineStore('tasks', () => {
  const operations = useOperationStore()
  const projects = useProjectStore()
  const tasks = ref<TaskSummary[]>([])
  const snapshots = ref<Record<string, TaskSnapshot>>({})
  const connected = ref(false)
  const loading = ref(false)
  const error = ref<string | null>(null)
  /** Which tasks `tasks` holds: live ones (default), archived ones, or both. */
  const listFilter = ref<TaskListFilter>('exclude')
  let disconnect: (() => void) | null = null
  /** Bumped whenever any task changes (a pushed or fetched snapshot, a removal), for views that
   *  derive cross-task data from the server, like the Decisions inbox. */
  const changes = ref(0)

  const listed = (task: TaskSummary): boolean =>
    listFilter.value === 'include' || (listFilter.value === 'only') === !!task.archivedAt

  function applySnapshot(snapshot: TaskSnapshot) {
    changes.value += 1
    snapshots.value = { ...snapshots.value, [snapshot.jiraKey]: snapshot }
    const index = tasks.value.findIndex((t) => t.jiraKey === snapshot.jiraKey)
    const summary: TaskSummary = { ...snapshot }
    // Archiving pushes an ordinary snapshot, so membership is decided here, not only by the server's list.
    if (!listed(summary)) {
      if (index >= 0) tasks.value.splice(index, 1)
    } else if (index >= 0) tasks.value.splice(index, 1, summary)
    else tasks.value.unshift(summary)
  }

  function removeTask(key: string) {
    changes.value += 1
    const rest = { ...snapshots.value }
    delete rest[key]
    snapshots.value = rest
    tasks.value = tasks.value.filter((t) => t.jiraKey !== key)
  }

  function connect() {
    if (disconnect) return
    disconnect = connectEvents(applySnapshot, (state) => {
      connected.value = state;
      if (state) {
        void loadTasks();
        // Project changes made while the socket was down were never pushed.
        void projects.load();
        for (const key of Object.keys(snapshots.value)) {
          // A 404 means it was deleted while the socket was down.
          void loadTask(key).catch((e) => { if (e instanceof ApiError && e.status === 404) removeTask(key) });
        }
      }
    }, removeTask, (event) => operations.progress(event.operation, event.jiraKey, event.message),
    (list) => projects.apply(list));
  }

  const stopConnection = () => {
    disconnect?.();
    disconnect = null;
    connected.value = false;
  };

  async function loadTasks(archived?: TaskListFilter) {
    if (archived) listFilter.value = archived
    loading.value = true
    try {
      tasks.value = await api.tasks(listFilter.value)
      error.value = null
    } catch (e) {
      error.value = (e as Error).message
    } finally {
      loading.value = false
    }
  }

  async function loadTask(key: string) {
    loading.value = true
    try {
      applySnapshot(await api.task(key))
      error.value = null
    } catch (e) {
      error.value = (e as Error).message
      throw e
    } finally {
      loading.value = false
    }
  }

  async function withSnapshot(action: () => Promise<TaskSnapshot>) {
    const snapshot = await action()
    applySnapshot(snapshot)
    return snapshot
  }

  const createTask = (payload: CreateTaskPayload, operationId?: string) =>
    withSnapshot(() => api.createTask(payload, operationId))
  const runNode = (key: string, node: string, override?: AgentOverride | null, meteredApproval?: MeteredApproval) =>
    withSnapshot(() => api.runNode(key, node, override, meteredApproval))
  const approveNode = (key: string, node: string, reason?: string) =>
    withSnapshot(() => api.approveNode(key, node, snapshots.value[key]?.currentSha, reason,
        snapshots.value[key]?.requirementsDraftDigest));
  const requestRequirementsChanges = (key: string, feedback: string) =>
    withSnapshot(() => api.requestRequirementsChanges(key, feedback));
  const overrideReviews = (key: string, reason: string) =>
    withSnapshot(() => api.overrideReviews(key, snapshots.value[key]?.currentSha, reason));
  const discardWorktreeDrift = (key: string) =>
    withSnapshot(() => api.discardWorktreeDrift(key, snapshots.value[key]?.currentSha ?? ''));
  const retryWorktreeSetup = (key: string) => withSnapshot(() => api.retryWorktreeSetup(key));
  const markPrCreated = (key: string, prUrl: string) => withSnapshot(() => api.markPrCreated(key, prUrl));
  const revertPrCreated = (key: string) => withSnapshot(() => api.revertPrCreated(key));
  const markPrMerged = (key: string) => withSnapshot(() => api.markPrMerged(key));
  const revertPrMerged = (key: string) => withSnapshot(() => api.revertPrMerged(key));
  /** Returns GitHub's new-PR form URL. */
  async function openPr(key: string) {
    const { url, task } = await api.openPr(key)
    applySnapshot(task)
    return url
  }
  const retargetPr = (key: string) => withSnapshot(() => api.retargetPr(key));
  const recoverNode = (key: string, node: string, retry: boolean) =>
    withSnapshot(() => api.recoverNode(key, node, retry));
  const setNodeConfig = (key: string, node: string, override: AgentOverride) =>
    withSnapshot(() => api.setNodeConfig(key, node, override))
  const cancelTask = (key: string) => withSnapshot(() => api.cancelTask(key))
  const archiveTask = (key: string) => withSnapshot(() => api.archiveTask(key))
  const unarchiveTask = (key: string) => withSnapshot(() => api.unarchiveTask(key))
  const setTaskProject = (key: string, projectId: number | null) =>
    withSnapshot(() => api.setTaskProject(key, projectId))
  const deleteTaskPreview = (key: string) => api.deleteTaskPreview(key)
  const cleanupWorktree = (key: string, expectedSha: string, force: boolean, operationId?: string) =>
    withSnapshot(() => api.cleanupWorktree(key, expectedSha, force, operationId))
  const deleteTask = async (key: string, options: DeleteTaskOptions, operationId?: string) => {
    const result = await api.deleteTask(key, options, operationId)
    removeTask(key)
    return result
  }
  const answerQuestion = (key: string, questionUid: string, body: AnswerBody) =>
    withSnapshot(() => api.answerQuestion(key, questionUid, body));
  const openZellij = async (key: string, node?: string | null) => {
    const result = await api.openZellij(key, node);
    window.dispatchEvent(
      new CustomEvent('agentflow:open-terminal', { detail: { jiraKey: key, nodeKey: node } }),
    );
    return result;
  };
  const openZellijWindow = (key: string, node?: string | null) => api.openZellijWindow(key, node);
  const revealPath = (key: string, target: 'worktree' | 'harnessCwd') => api.revealPath(key, target);

  const byKey = computed(() => (key: string) => snapshots.value[key])

  return {
    tasks, snapshots, connected, loading, error, byKey, listFilter, changes,
    connect, stopConnection, loadTasks, loadTask, createTask, runNode, approveNode,
    requestRequirementsChanges,
    overrideReviews, discardWorktreeDrift, markPrCreated, revertPrCreated, markPrMerged, revertPrMerged,
    openPr, retargetPr, recoverNode, setNodeConfig, cancelTask, archiveTask, unarchiveTask, setTaskProject, deleteTaskPreview, deleteTask,
    cleanupWorktree,
    answerQuestion, openZellij, openZellijWindow, revealPath, retryWorktreeSetup,
  }
})
