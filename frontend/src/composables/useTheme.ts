import { computed, ref } from 'vue'

export type Theme = 'light' | 'dark'

const STORAGE_KEY = 'agentflow-theme'

// The inline script in index.html has already applied the stored (or system) theme before the app
// mounts, so the attribute is the source of truth for the initial value.
const readApplied = (): Theme =>
  document.documentElement.getAttribute('data-theme') === 'dark' ? 'dark' : 'light'

const theme = ref<Theme>(readApplied())

const persist = (value: Theme): void => {
  try {
    localStorage.setItem(STORAGE_KEY, value)
  } catch {
    // Storage may be unavailable (private browsing); the theme still applies for this session.
  }
}

export function useTheme() {
  const isDark = computed(() => theme.value === 'dark')

  const toggle = (): void => {
    theme.value = isDark.value ? 'light' : 'dark'
    document.documentElement.setAttribute('data-theme', theme.value)
    persist(theme.value)
  }

  return { theme, isDark, toggle }
}
