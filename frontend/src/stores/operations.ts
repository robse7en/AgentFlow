import { defineStore } from 'pinia'
import { ref } from 'vue'
import { useToastStore, type ToastAction } from './toasts'

/** Long requests the UI no longer waits on: the page moves on and a toast follows them. */
export type OperationKind = 'create' | 'cleanup' | 'delete'

interface Operation {
  id: string
  kind: OperationKind
  jiraKey: string | null
  toastId: number
}

export interface Outcome {
  /** A success that needs reading (it stays until dismissed). */
  warning?: boolean
  title: string
  message?: string
  actions?: ToastAction[]
}

export interface StartOptions<T> {
  kind: OperationKind
  /** Null while a local task has no key yet (the server picks it). */
  jiraKey: string | null
  title: string
  /** The request, sent with the operation id so the server's progress steps find this toast. */
  call: (operationId: string) => Promise<T>
  success: (result: T) => Outcome
  failure: (error: unknown) => Outcome
}

// Not crypto.randomUUID: a browser paired over the LAN isn't a secure context.
const newId = () => `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`

export const useOperationStore = defineStore('operations', () => {
  const toasts = useToastStore()
  const running = ref<Operation[]>([])

  /** Start `call` and return at once; the toast reports its steps and outcome. */
  function start<T>(options: StartOptions<T>): void {
    const operation: Operation = {
      id: newId(),
      kind: options.kind,
      jiraKey: options.jiraKey,
      toastId: toasts.show('progress', options.title, 'Starting…'),
    }
    running.value = [...running.value, operation]
    const finish = (outcome: Outcome, kind: 'success' | 'warning' | 'error') => {
      running.value = running.value.filter((o) => o.id !== operation.id)
      toasts.update(operation.toastId, {
        kind, title: outcome.title, message: outcome.message ?? '', actions: outcome.actions ?? [],
      })
    }
    options.call(operation.id).then(
      (result) => {
        const outcome = options.success(result)
        finish(outcome, outcome.warning ? 'warning' : 'success')
      },
      (error: unknown) => finish(options.failure(error), 'error'),
    )
  }

  /** A server progress step; ignored once the operation has finished (the reply can win the race). */
  function progress(operationId: string, jiraKey: string | null, message: string) {
    const operation = running.value.find((o) => o.id === operationId)
    if (!operation) return
    if (!operation.jiraKey && jiraKey) operation.jiraKey = jiraKey
    toasts.update(operation.toastId, { message })
  }

  /** The operation still running for this task, if any (one task's actions wait on its lock). */
  const runningFor = (jiraKey: string, kind?: OperationKind): Operation | undefined =>
    running.value.find((o) => o.jiraKey === jiraKey && (!kind || o.kind === kind))

  return { running, start, progress, runningFor }
})
