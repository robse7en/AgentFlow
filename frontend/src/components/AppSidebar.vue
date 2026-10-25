<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import { useTaskStore } from '@/stores/tasks'
import { useDecisionStore } from '@/stores/decisions'
import { usePersistedFlag } from '@/composables/usePersistedFlag'
import { useTheme } from '@/composables/useTheme'

interface NavItem {
  to: string
  label: string
  icon: string
  routeNames: string[]
}

const NAV_ITEMS: NavItem[] = [
  { to: '/', label: 'Tasks', icon: 'M4 5h7v7H4zM13 5h7v4h-7zM13 11h7v8h-7zM4 14h7v5H4z', routeNames: ['home', 'task'] },
  {
    to: '/projects',
    label: 'Projects',
    icon: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
    routeNames: ['projects'],
  },
  {
    to: '/decisions',
    label: 'Decisions',
    icon: 'M2.8 12a2.2 2.2 0 1 0 4.4 0a2.2 2.2 0 1 0-4.4 0M15.8 6a2.2 2.2 0 1 0 4.4 0a2.2 2.2 0 1 0-4.4 0M15.8 18a2.2 2.2 0 1 0 4.4 0a2.2 2.2 0 1 0-4.4 0M7.2 12H11c2 0 2-6 4.8-6M11 12c2 0 2 6 4.8 6',
    routeNames: ['decisions'],
  },
  { to: '/tasks/new', label: 'New Task', icon: 'M12 5v14M5 12h14', routeNames: ['new-task'] },
  {
    to: '/settings',
    label: 'Settings',
    icon: 'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19 12a7 7 0 0 0-.1-1.2l2-1.5-2-3.4-2.3.9a7 7 0 0 0-2-1.2L14 3h-4l-.6 2.6a7 7 0 0 0-2 1.2l-2.3-.9-2 3.4 2 1.5A7 7 0 0 0 5 12c0 .4 0 .8.1 1.2l-2 1.5 2 3.4 2.3-.9a7 7 0 0 0 2 1.2L10 21h4l.6-2.6a7 7 0 0 0 2-1.2l2.3.9 2-3.4-2-1.5c.1-.4.1-.8.1-1.2z',
    routeNames: ['settings'],
  },
]
const SUN_ICON = 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4'
const MOON_ICON = 'M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z'
const appVersion = __APP_VERSION__

const route = useRoute()
const tasks = useTaskStore()
const decisions = useDecisionStore()
const { isDark, toggle: toggleTheme } = useTheme()
const collapsed = usePersistedFlag('agentflow-sidebar-collapsed', false)
// On a narrow window the full sidebar would leave the page too little room, so it shows as the
// icon rail whatever the saved choice (which comes back once the window is wide again).
const narrowQuery = window.matchMedia('(max-width: 700px)')
const narrow = ref(narrowQuery.matches)
const onNarrowChange = (event: MediaQueryListEvent) => { narrow.value = event.matches }
onMounted(() => narrowQuery.addEventListener('change', onNarrowChange))
onUnmounted(() => narrowQuery.removeEventListener('change', onNarrowChange))

const isActive = (item: NavItem) => item.routeNames.includes(String(route.name))
const themeLabel = computed(() => (isDark.value ? 'Light mode' : 'Dark mode'))
</script>

<template>
  <aside class="sidebar" :class="{ collapsed: collapsed || narrow }" aria-label="Sidebar">
    <RouterLink to="/" class="brand">
      <span class="logo" aria-hidden="true">
        <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M4 17c4 0 4-10 8-10s4 10 8 10" /></svg>
      </span>
      <span class="label brand-name">AgentFlow</span>
    </RouterLink>

    <nav aria-label="Main">
      <RouterLink v-for="item in NAV_ITEMS" :key="item.to" :to="item.to" class="nav-item" :class="{ active: isActive(item) }" :title="item.label">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path :d="item.icon" /></svg>
        <span class="label">{{ item.label }}</span>
        <span
          v-if="item.to === '/decisions' && decisions.waiting"
          :key="decisions.waiting"
          class="nav-count"
          :aria-label="`${decisions.waiting} waiting`"
        >{{ decisions.waiting }}</span>
      </RouterLink>
    </nav>

    <footer class="sidebar-footer">
      <button type="button" class="theme-button" :title="themeLabel" :aria-label="themeLabel" @click="toggleTheme">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path :d="isDark ? SUN_ICON : MOON_ICON" /></svg>
      </button>
      <span v-if="appVersion" class="version mono" :title="`AgentFlow ${appVersion}`">v{{ appVersion }}</span>
      <span class="live small muted" :title="tasks.connected ? 'Live updates connected' : 'Live updates disconnected'">
        <span class="dot" :class="tasks.connected ? 'on' : 'off'" />
        <span class="label">{{ tasks.connected ? 'live' : 'offline' }}</span>
      </span>
    </footer>

    <button
      v-if="!narrow"
      type="button"
      class="collapse-toggle"
      :aria-expanded="!collapsed"
      aria-label="Collapse or expand sidebar"
      @click="collapsed = !collapsed"
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 6 6 6-6 6" /></svg>
    </button>
  </aside>
</template>

<style scoped>
.sidebar {
  position: relative;
  z-index: 20;
  flex: 0 0 var(--sidebar-width);
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  padding: 1.5rem 1rem;
  background: var(--panel);
  border-right: 1px solid var(--border);
  transition: flex-basis 0.45s var(--spring), padding 0.45s var(--spring);
}
.sidebar.collapsed { flex-basis: var(--sidebar-width-collapsed); padding-inline: 0.75rem; }

.brand {
  display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.25rem; padding-inline: 0.4rem;
  color: var(--text); text-decoration: none;
}
.logo {
  flex: none; width: 40px; height: 40px; display: grid; place-items: center;
  border-radius: var(--radius-md); color: var(--on-btn);
  background: linear-gradient(135deg, var(--btn-from), var(--btn-to));
  box-shadow: 0 8px 24px var(--accent-soft);
}
.brand-name { font-weight: 700; font-size: 1.1rem; letter-spacing: -0.02em; }

nav { display: flex; flex-direction: column; gap: 0.25rem; }

.nav-item {
  display: flex; align-items: center; gap: 0.75rem; width: 100%; min-height: 44px; padding: 0 0.9rem;
  border: 0; border-radius: var(--radius-md); background: transparent; color: var(--muted);
  font: inherit; font-weight: 500; text-align: left; text-decoration: none; cursor: pointer;
  transition: background 0.25s var(--spring), color 0.25s, transform 0.25s var(--spring);
}
.nav-item svg { flex: none; transition: transform 0.4s var(--spring); }
.nav-item:hover { background: var(--hover); color: var(--text); text-decoration: none; transform: translateX(3px); }
.nav-item:hover svg { transform: scale(1.1); }
.nav-item { position: relative; }
/* Questions waiting across tasks; bumps whenever the number changes (re-keyed). */
.nav-count {
  margin-left: auto; flex: none; min-width: 22px; box-sizing: border-box; padding: 2px 8px; border-radius: var(--radius-pill);
  background: var(--warn-soft); color: var(--warn); font-size: 11.5px; font-weight: 700; text-align: center;
  animation: nav-count-bump 0.45s var(--spring);
}
.collapsed .nav-count { position: absolute; top: 4px; right: 10px; min-width: 18px; padding: 1px 5px; font-size: 10.5px; }
@keyframes nav-count-bump { 0% { transform: scale(1); } 35% { transform: scale(1.25); } 100% { transform: scale(1); } }
.nav-item.active { background: var(--accent-soft); color: var(--accent-strong); box-shadow: inset 0 0 0 1px var(--accent-border); }

/* Pinned to the bottom: theme toggle on the left, live-updates status on the right. */
.sidebar-footer { display: flex; align-items: center; justify-content: space-between; gap: 0.5rem; margin-top: auto; padding-inline: 0.4rem; }
.theme-button {
  flex: none; width: 44px; height: 44px; padding: 0; display: grid; place-items: center;
  border: 0; border-radius: var(--radius-md); background: transparent; color: var(--muted);
}
.theme-button:hover:not(:disabled) { background: var(--hover); color: var(--text); }
.theme-button svg { transition: transform 0.5s var(--spring); }
.theme-button:hover svg { transform: rotate(25deg); }
.version { font-size: 11.5px; color: var(--muted); white-space: nowrap; }
.live { display: flex; align-items: center; gap: 0.5rem; white-space: nowrap; }
.live .dot { margin-right: 0; flex: none; }
.live .dot.on { animation: live-pulse 1.8s ease-out infinite; }

.label { white-space: nowrap; overflow: hidden; max-width: 200px; transition: opacity 0.25s, max-width 0.45s var(--spring); }
.collapsed .label { opacity: 0; max-width: 0; }
.collapsed .brand { justify-content: center; gap: 0; padding-inline: 0; }
.collapsed .nav-item { justify-content: center; gap: 0; padding-inline: 0; }
.collapsed .nav-item:hover { transform: none; }
/* The rail is too narrow for a row: the footer stacks, the dot under the toggle. */
.collapsed .sidebar-footer { flex-direction: column; gap: 0.75rem; padding-inline: 0; }
.collapsed .live { justify-content: center; gap: 0; }

.collapse-toggle {
  position: absolute; top: 34px; right: -14px; z-index: 30; width: 28px; height: 28px; padding: 0;
  display: grid; place-items: center; border-radius: 50%; color: var(--text); background: var(--panel);
  box-shadow: 0 4px 12px var(--shadow-1);
}
.collapse-toggle::after { content: ""; position: absolute; inset: -8px; }
.collapse-toggle svg { transform: rotate(180deg); transition: transform 0.45s var(--spring); }
.collapsed .collapse-toggle svg { transform: none; }

@keyframes live-pulse {
  0% { box-shadow: 0 0 0 0 var(--ok-border); }
  100% { box-shadow: 0 0 0 8px transparent; }
}
@media (prefers-reduced-motion: reduce) {
  .sidebar, .label, .nav-item, .nav-item svg, .theme-button svg, .collapse-toggle svg { transition: none; }
  .live .dot.on { animation: none; }
  .nav-count { animation: none; }
}
</style>
