<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import type { Project } from '@/api/types'
import { useProjectStore } from '@/stores/projects'
import { NO_PROJECT, projectColor } from '@/projects'

// `modelValue`: the `?project=` slug, NO_PROJECT, or null for every project. `counts`: tasks per
// project id (null: no project) among those the other filters leave.
const props = defineProps<{ modelValue: string | null; counts: Map<number | null, number>; total: number }>()
const emit = defineEmits<{ 'update:modelValue': [slug: string | null] }>()

const projects = useProjectStore()
const open = ref(false)
const root = ref<HTMLElement | null>(null)

const selected = computed<Project | null>(() =>
  props.modelValue && props.modelValue !== NO_PROJECT ? projects.bySlug.get(props.modelValue) ?? null : null)
const label = computed(() => (props.modelValue === NO_PROJECT ? 'No project' : selected.value?.name ?? 'All projects'))
// Archived projects are listed only while they still have tasks here.
const archived = computed(() => projects.archived.filter((p) => props.counts.get(p.id)))

const close = (): void => { open.value = false }
const closeOnOutsidePress = (event: PointerEvent): void => {
  if (root.value && !root.value.contains(event.target as Node)) close()
}
const closeOnEscape = (event: KeyboardEvent): void => {
  if (event.key === 'Escape') close()
}
const stopListening = (): void => {
  document.removeEventListener('pointerdown', closeOnOutsidePress)
  document.removeEventListener('keydown', closeOnEscape)
}
watch(open, (isOpen) => {
  if (!isOpen) return stopListening()
  document.addEventListener('pointerdown', closeOnOutsidePress)
  document.addEventListener('keydown', closeOnEscape)
})
onBeforeUnmount(stopListening)

function choose(slug: string | null) {
  close()
  emit('update:modelValue', slug)
}
</script>

<template>
  <div ref="root" class="project-filter">
    <button
      type="button" class="trigger" :class="{ active: modelValue }" aria-haspopup="menu" :aria-expanded="open"
      :aria-label="`Project: ${label}`" @click="open = !open"
    >
      <span v-if="selected" class="project-dot" :style="{ '--dot': projectColor(selected.color) }" aria-hidden="true" />
      <span v-else-if="modelValue === NO_PROJECT" class="project-dot none" aria-hidden="true" />
      <svg v-else width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /></svg>
      <span class="name">{{ label }}</span>
      <svg class="caret" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 9 6 6 6-6" /></svg>
    </button>
    <Transition name="menu-pop">
      <div v-if="open" class="menu" role="menu" aria-label="Filter by project">
        <button type="button" role="menuitemradio" :aria-checked="!modelValue" class="item" :class="{ on: !modelValue }" @click="choose(null)">
          <span class="label">All projects</span><span class="n">{{ total }}</span>
        </button>
        <button
          v-for="p in projects.active" :key="p.id" type="button" role="menuitemradio" :aria-checked="modelValue === p.slug"
          class="item" :class="{ on: modelValue === p.slug }" @click="choose(p.slug)"
        >
          <span class="project-dot" :style="{ '--dot': projectColor(p.color) }" aria-hidden="true" />
          <span class="label">{{ p.name }}</span><span class="n">{{ counts.get(p.id) ?? 0 }}</span>
        </button>
        <button type="button" role="menuitemradio" :aria-checked="modelValue === NO_PROJECT" class="item" :class="{ on: modelValue === NO_PROJECT }" @click="choose(NO_PROJECT)">
          <span class="project-dot none" aria-hidden="true" />
          <span class="label">No project</span><span class="n">{{ counts.get(null) ?? 0 }}</span>
        </button>
        <template v-if="archived.length">
          <div class="group" role="presentation">Archived projects</div>
          <button
            v-for="p in archived" :key="p.id" type="button" role="menuitemradio" :aria-checked="modelValue === p.slug"
            class="item" :class="{ on: modelValue === p.slug }" @click="choose(p.slug)"
          >
            <span class="project-dot" :style="{ '--dot': projectColor(p.color) }" aria-hidden="true" />
            <span class="label">{{ p.name }}</span><span class="n">{{ counts.get(p.id) ?? 0 }}</span>
          </button>
        </template>
        <hr />
        <RouterLink to="/projects" role="menuitem" class="item link" @click="close">Manage projects…</RouterLink>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.project-filter { position: relative; flex: none; }
.trigger {
  display: inline-flex; align-items: center; gap: 0.5rem; min-height: 44px; max-width: 260px; padding: 0 1rem;
  border: 0; background: var(--info-soft); color: var(--muted);
}
.trigger:hover:not(:disabled) { color: var(--text); background: var(--info-soft); }
.trigger.active { color: var(--accent-strong); background: var(--accent-soft); box-shadow: inset 0 0 0 1px var(--accent-border); }
.trigger .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.trigger .caret { flex: none; opacity: 0.7; }

.menu {
  position: absolute; left: 0; top: calc(100% + 6px); z-index: 50; width: max-content; min-width: 240px; max-width: min(320px, 90vw);
  max-height: min(420px, 70vh); overflow-y: auto; padding: 0.4rem; display: flex; flex-direction: column; transform-origin: top left;
  background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius-md); box-shadow: var(--elev);
}
.item {
  display: flex; align-items: center; gap: 0.6rem; min-height: 40px; padding: 0.3rem 0.75rem;
  border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--text); text-align: left; text-decoration: none;
}
.item:hover:not(:disabled) { background: var(--hover); transform: none; text-decoration: none; }
.item.on { background: var(--hover); font-weight: 600; }
.item .label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.item .n { color: var(--muted); font-size: 12px; font-variant-numeric: tabular-nums; font-weight: 400; }
.item.link { color: var(--accent-strong); font-weight: 500; }
.group { padding: 0.5rem 0.75rem 0.2rem; font-size: 11px; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; color: var(--muted); }
hr { border: 0; border-top: 1px solid var(--border); margin: 0.3rem 0.25rem; }

.menu-pop-enter-active { transition: opacity 0.2s, transform 0.3s var(--spring); }
.menu-pop-leave-active { transition: opacity 0.12s; }
.menu-pop-enter-from { opacity: 0; transform: scale(0.9) translateY(-6px); }
.menu-pop-leave-to { opacity: 0; }
@media (prefers-reduced-motion: reduce) { .menu-pop-enter-active, .menu-pop-leave-active { transition: none; } }
</style>
