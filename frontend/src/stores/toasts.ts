import { defineStore } from 'pinia'
import { ref } from 'vue'

/** `warning` is a success with something to read (e.g. files discarded): it stays like an error. */
export type ToastKind = 'progress' | 'success' | 'warning' | 'error'

export interface ToastAction {
  label: string
  run: () => void
}

export interface Toast {
  id: number
  kind: ToastKind
  title: string
  message: string
  actions: ToastAction[]
}

/** Successes clear themselves; warnings and errors stay until dismissed, since they carry something to act on. */
const SUCCESS_MS = 6000

export const useToastStore = defineStore('toasts', () => {
  const toasts = ref<Toast[]>([])
  const timers = new Map<number, number>()
  let nextId = 1

  function show(kind: ToastKind, title: string, message = '', actions: ToastAction[] = []): number {
    const id = nextId++
    toasts.value = [...toasts.value, { id, kind, title, message, actions }]
    schedule(id, kind)
    return id
  }

  /** Change a toast in place (a progress toast becoming its outcome keeps its position). */
  function update(id: number, changes: Partial<Omit<Toast, 'id'>>) {
    const index = toasts.value.findIndex((t) => t.id === id)
    if (index < 0) return
    const next = { ...toasts.value[index], ...changes }
    toasts.value = toasts.value.map((t) => (t.id === id ? next : t))
    if (changes.kind) schedule(id, changes.kind)
  }

  function dismiss(id: number) {
    window.clearTimeout(timers.get(id))
    timers.delete(id)
    toasts.value = toasts.value.filter((t) => t.id !== id)
  }

  function schedule(id: number, kind: ToastKind) {
    window.clearTimeout(timers.get(id))
    timers.delete(id)
    if (kind === 'success') timers.set(id, window.setTimeout(() => dismiss(id), SUCCESS_MS))
  }

  return { toasts, show, update, dismiss }
})
