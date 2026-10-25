import { defineStore } from 'pinia'
import { computed, ref, watch } from 'vue'
import { api } from '@/api/client'
import type { OpenQuestionDto } from '@/api/types'
import { inboxOrder } from '@/decisions'
import { useTaskStore } from './tasks'

/** Open agent questions across every task, for the Decisions inbox and the sidebar's count.
 *  Reloaded (debounced) whenever the live task feed reports a change. */
export const useDecisionStore = defineStore('decisions', () => {
  const tasks = useTaskStore()
  const items = ref<OpenQuestionDto[]>([])
  const loaded = ref(false)
  const error = ref<string | null>(null)
  let timer: ReturnType<typeof setTimeout> | null = null
  let started = false

  async function load() {
    try {
      items.value = inboxOrder(await api.openQuestions())
      error.value = null
    } catch (cause) {
      error.value = (cause as Error).message
    } finally {
      loaded.value = true
    }
  }

  const reloadSoon = () => {
    if (timer) clearTimeout(timer)
    timer = setTimeout(() => { timer = null; void load() }, 250)
  }

  /** Start following the task feed; safe to call from every component that shows decisions. */
  function start() {
    if (started) return
    started = true
    void load()
    watch(() => tasks.changes, reloadSoon)
    watch(() => tasks.connected, (connected) => { if (connected) reloadSoon() })
  }

  const waiting = computed(() => items.value.length)
  return { items, loaded, error, waiting, load, start }
})
