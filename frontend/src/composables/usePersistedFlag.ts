import { ref, watch, type Ref } from 'vue'

// A boolean that survives reloads. Storage can be unavailable (private browsing, quota), in which
// case the flag still works for the current session.
export function usePersistedFlag(storageKey: string, initial: boolean): Ref<boolean> {
  const flag = ref(read(storageKey, initial))
  watch(flag, (value) => write(storageKey, value))
  return flag
}

function read(storageKey: string, fallback: boolean): boolean {
  try {
    const stored = localStorage.getItem(storageKey)
    return stored === null ? fallback : stored === 'true'
  } catch {
    return fallback
  }
}

function write(storageKey: string, value: boolean): void {
  try {
    localStorage.setItem(storageKey, String(value))
  } catch {
    // Not persisted; the choice still applies until the page is closed.
  }
}
