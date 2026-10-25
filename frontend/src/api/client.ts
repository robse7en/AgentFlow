import type {
  AgentOverride,
  AnswerBody,
  BudgetGroup,
  MeteredApproval,
  CreateTaskPayload,
  DeleteTaskOptions,
  DeleteTaskPreview,
  DirectoryListing,
  HarnessInfo,
  JiraSearchResult,
  OpenQuestionDto,
  Project,
  ProjectPayload,
  RepositoryValidation,
  SystemConfig,
  TaskSnapshot,
  TaskListFilter,
  TaskSummary,
  WorktreeCleanupPreview,
} from './types'

export function jiraTicketUrl(baseUrl: string | null | undefined, jiraKey: string): string | null {
  if (!baseUrl) return null
  return `${baseUrl.replace(/\/+$/, '')}/browse/${jiraKey}`
}

export class ApiError extends Error {
  status: number
  /** Stable tag from the backend (WorkflowError.code) for errors the UI reacts to specially. */
  code: string | null
  constructor(status: number, message: string, code: string | null = null) {
    super(message)
    this.status = status
    this.code = code
  }
}

export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    ...init,
    headers: { 'Content-Type': 'application/json', ...(init?.headers ?? {}) },
  })
  if (!response.ok) {
    if (response.status === 401) window.dispatchEvent(new Event('agentflow:unpaired'));
    let detail = response.statusText
    let code: string | null = null
    try {
      const body = await response.json()
      detail = typeof body.detail === 'string' ? body.detail : JSON.stringify(body.detail ?? body)
      if (typeof body.code === 'string') code = body.code
    } catch {
      /* not JSON */
    }
    throw new ApiError(response.status, detail, code)
  }
  return (await response.json()) as T
}

/** `operationId` tags a long request so the server's progress steps reach the toast that started it. */
const post = <T>(path: string, body?: unknown, operationId?: string) =>
  request<T>(path, {
    method: 'POST',
    body: body === undefined ? undefined : JSON.stringify(body),
    headers: operationId ? { 'X-Operation-Id': operationId } : undefined,
  })

export const api = {
  health: () => request<{ ok: boolean; version: string }>('/api/health'),
  config: () => request<SystemConfig>('/api/config'),
  costs: () => request<{ daily: BudgetGroup; dayUtc: string }>('/api/costs'),
  setCostControls: (controls: unknown, billing: Record<string, string>) =>
    request<SystemConfig>('/api/config/cost-controls', {
        method: 'PUT', body: JSON.stringify({ controls, billing }),
    }),
  setTaskBudget: (key: string, amount: number | null) =>
    request<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/budget`, {
        method: 'PUT', body: JSON.stringify({ amount }),
    }),
  importUsage: (key: string, runId: number, report: unknown) =>
    request<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/runs/${runId}/usage`, {
        method: 'PUT', body: JSON.stringify(report),
    }),
  reloadConfig: () => post<SystemConfig>('/api/config/reload'),
  setDefaultRepository: (path: string | null) =>
    request<SystemConfig>('/api/config/default-repository', { method: 'PUT', body: JSON.stringify({ path }) }),
  setExchangeRate: (usdToNzdRate: number) =>
    request<SystemConfig>('/api/config/exchange-rate', { method: 'PUT', body: JSON.stringify({ usdToNzdRate }) }),
  harnesses: () => request<HarnessInfo[]>('/api/harnesses'),
  detectHarnesses: () => post<HarnessInfo[]>('/api/harnesses/detect'),

  searchJira: (query: string) =>
    request<JiraSearchResult[]>(`/api/jira/search?query=${encodeURIComponent(query)}`),

  listDirectory: (path?: string | null, root?: string | null) => {
    const params = new URLSearchParams()
    if (path) params.set('path', path)
    if (root) params.set('root', root)
    const query = params.toString()
    return request<DirectoryListing>(`/api/fs/list${query ? `?${query}` : ''}`)
  },

  tasks: (archived: TaskListFilter = 'exclude') =>
    request<TaskSummary[]>(archived === 'exclude' ? '/api/tasks' : `/api/tasks?archived=${archived}`),
  task: (key: string) => request<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}`),
  createTask: (payload: CreateTaskPayload, operationId?: string) =>
    post<TaskSnapshot>('/api/tasks', payload, operationId),
  validateRepository: (path: string, baseBranch?: string | null, projectRelativePath?: string | null) =>
    post<RepositoryValidation>('/api/tasks/validate-repository', { path, baseBranch, projectRelativePath }),
  cancelTask: (key: string) => post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/cancel`),
  archiveTask: (key: string) => post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/archive`),
  unarchiveTask: (key: string) => post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/unarchive`),
  setTaskProject: (key: string, projectId: number | null) =>
    request<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/project`, {
      method: 'PUT', body: JSON.stringify({ projectId }),
    }),

  projects: () => request<Project[]>('/api/projects'),
  createProject: (payload: ProjectPayload) => post<Project>('/api/projects', payload),
  updateProject: (id: number, payload: ProjectPayload) =>
    request<Project>(`/api/projects/${id}`, { method: 'PATCH', body: JSON.stringify(payload) }),
  archiveProject: (id: number) => post<Project>(`/api/projects/${id}/archive`),
  unarchiveProject: (id: number) => post<Project>(`/api/projects/${id}/unarchive`),
  deleteProject: (id: number) =>
    request<{ deleted: number; unassignedTasks: string[] }>(`/api/projects/${id}`, { method: 'DELETE' }),
  deleteTaskPreview: (key: string) =>
    request<DeleteTaskPreview>(`/api/tasks/${encodeURIComponent(key)}/delete-preview`),
  deleteTask: (key: string, options: DeleteTaskOptions, operationId?: string) =>
    post<{ jiraKey: string; deleted: boolean }>(`/api/tasks/${encodeURIComponent(key)}/delete`, options, operationId),
  worktreeCleanupPreview: (key: string) =>
    request<WorktreeCleanupPreview>(`/api/tasks/${encodeURIComponent(key)}/worktree/cleanup-preview`),
  cleanupWorktree: (key: string, expectedSha: string, force = false, operationId?: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/worktree/cleanup`, { expectedSha, force }, operationId),
  retryWorktreeSetup: (key: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/worktree/setup/retry`),
  discardWorktreeDrift: (key: string, expectedSha: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/worktree/discard-drift`, { expectedSha }),
  markPrCreated: (key: string, prUrl: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/pr/created`, { prUrl }),
  revertPrCreated: (key: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/pr/created/revert`),
  markPrMerged: (key: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/pr/merged`),
  revertPrMerged: (key: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/pr/merged/revert`),
  /** Pushes the branch; `url` is GitHub's new-PR form already targeting the task's base branch. */
  openPr: (key: string) =>
    post<{ url: string, task: TaskSnapshot }>(`/api/tasks/${encodeURIComponent(key)}/pr/open`),
  retargetPr: (key: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/pr/retarget`),
  /** The server builds the line typed into the agent's pane from the stored question. */
  answerQuestion: (key: string, questionUid: string, body: AnswerBody) =>
    post<TaskSnapshot>(
      `/api/tasks/${encodeURIComponent(key)}/questions/${encodeURIComponent(questionUid)}/answer`,
      body,
    ),
  openQuestions: () => request<OpenQuestionDto[]>('/api/questions?open=true'),

  runNode: (key: string, node: string, override?: AgentOverride | null, meteredApproval?: MeteredApproval) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/nodes/${node}/run`, {
        override: override ?? null, meteredApproval,
    }),
  approveNode: (key: string, node: string, expectedSha?: string | null, reason?: string,
    expectedRequirementsDigest?: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/nodes/${node}/approve`, {
      expectedSha, reason, expectedRequirementsDigest,
    }),
  requestRequirementsChanges: (key: string, feedback: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/nodes/requirements_gate/request-changes`, {
      feedback,
    }),
  overrideReviews: (key: string, expectedSha: string | null, reason: string) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/review-override`, {
      expectedSha, reason,
    }),
  recoverNode: (key: string, node: string, retry: boolean) =>
    post<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/nodes/${node}/recover`, { retry }),
  setNodeConfig: (key: string, node: string, override: AgentOverride) =>
    request<TaskSnapshot>(`/api/tasks/${encodeURIComponent(key)}/nodes/${node}/config`, {
      method: 'PUT',
      body: JSON.stringify(override),
    }),
  openZellij: (key: string, node?: string | null) =>
    post<{ message: string; session: string; attachCommand: string; webUrl: string | null }>(
      `/api/tasks/${encodeURIComponent(key)}/zellij/open${node ? `?node=${encodeURIComponent(node)}` : ''}`,
    ),
  openZellijWindow: (key: string, node?: string | null) =>
    post<{ session: string; tab: string | null }>(
      `/api/tasks/${encodeURIComponent(key)}/zellij/window${node ? `?node=${encodeURIComponent(node)}` : ''}`,
    ),
  revealPath: (key: string, target: 'worktree' | 'harnessCwd') =>
    post<{ path: string }>(`/api/tasks/${encodeURIComponent(key)}/reveal/${target}`),
}

/** A step a tagged long request has reached (see `post`'s operationId). */
export interface ProgressEvent {
  operation: string
  jiraKey: string | null
  message: string
}

/**
 * Live task updates: the backend pushes a full snapshot on every state change, the key of a deleted task, the
 * progress steps of long requests, and the whole project list whenever a project changes.
 */
export function connectEvents(onTask: (task: TaskSnapshot) => void, onStatus: (connected: boolean) => void,
  onTaskDeleted: (jiraKey: string) => void, onProgress?: (event: ProgressEvent) => void,
  onProjects?: (projects: Project[]) => void): () => void {
  let socket: WebSocket | null = null
  let closed = false
  let retry = 1000
  let keepAlive: number | undefined

  const open = () => {
    const protocol = location.protocol === 'https:' ? 'wss' : 'ws'
    socket = new WebSocket(`${protocol}://${location.host}/api/ws`)
    socket.onopen = () => {
      retry = 1000
      onStatus(true)
      keepAlive = window.setInterval(() => socket?.readyState === WebSocket.OPEN && socket.send('ping'), 20000)
    }
    socket.onmessage = (event) => {
      try {
        const message = JSON.parse(event.data)
        if (message.type === 'task' && message.task) onTask(message.task as TaskSnapshot)
        else if (message.type === 'task.deleted' && typeof message.jiraKey === 'string') onTaskDeleted(message.jiraKey)
        else if (message.type === 'progress' && typeof message.operation === 'string'
            && typeof message.message === 'string') onProgress?.(message as ProgressEvent)
        else if (message.type === 'projects' && Array.isArray(message.projects)) onProjects?.(message.projects as Project[])
      } catch {
        /* ignore malformed frames */
      }
    }
    socket.onclose = () => {
      onStatus(false)
      if (!closed) {
        void fetch('/api/auth/session').then((response) => {
          if (response.status === 401) window.dispatchEvent(new Event('agentflow:unpaired'));
        }).catch(() => undefined);
      }
      window.clearInterval(keepAlive)
      if (!closed) {
        window.setTimeout(() => { if (!closed) open(); }, retry)
        retry = Math.min(retry * 2, 15000)
      }
    }
    socket.onerror = () => socket?.close()
  }
  open()
  return () => {
    closed = true
    window.clearInterval(keepAlive)
    socket?.close()
  }
}
