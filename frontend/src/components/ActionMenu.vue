<script lang="ts">
export interface MenuItem {
  label: string
  hint?: string
  danger?: boolean
  disabled?: boolean
  run: () => void
}
</script>

<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue'

defineProps<{ items: MenuItem[]; label?: string }>()

const open = ref(false)
const root = ref<HTMLElement | null>(null)

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

const choose = (item: MenuItem): void => {
  close()
  item.run()
}
</script>

<template>
  <!-- Click is stopped so a menu inside a clickable row never triggers the row. -->
  <div ref="root" class="action-menu" @click.stop>
    <button type="button" class="trigger" :aria-label="label ?? 'More actions'" aria-haspopup="menu" :aria-expanded="open" @click="open = !open">
      <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><circle cx="5" cy="12" r="2" /><circle cx="12" cy="12" r="2" /><circle cx="19" cy="12" r="2" /></svg>
    </button>
    <Transition name="menu-pop">
      <div v-if="open" class="menu" role="menu">
        <button
          v-for="item in items"
          :key="item.label"
          type="button"
          role="menuitem"
          class="item"
          :class="{ danger: item.danger }"
          :disabled="item.disabled"
          @click="choose(item)"
        >
          {{ item.label }}
          <small v-if="item.hint">{{ item.hint }}</small>
        </button>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.action-menu { position: relative; flex: none; }
.trigger {
  width: 44px; height: 44px; padding: 0; display: grid; place-items: center;
  border: 0; border-radius: 50%; background: transparent; color: var(--muted);
}
.trigger:hover:not(:disabled) { background: var(--hover); color: var(--text); transform: scale(1.08); }

.menu {
  position: absolute; right: 0; top: calc(100% + 6px); z-index: 50; min-width: 220px; padding: 0.5rem;
  display: flex; flex-direction: column; transform-origin: top right;
  background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius-md); box-shadow: var(--elev);
}
.item {
  display: flex; flex-direction: column; align-items: flex-start; justify-content: center; min-height: 44px;
  padding: 0.4rem 0.9rem; border: 0; border-radius: var(--radius-sm); background: transparent; text-align: left;
  transition: background 0.2s, padding 0.25s var(--spring);
}
.item:hover:not(:disabled) { background: var(--hover); padding-left: 1.2rem; transform: none; }
.item.danger { color: var(--danger); }
.item small { color: var(--muted); font-size: 11.5px; }

.menu-pop-enter-active { transition: opacity 0.2s, transform 0.3s var(--spring); }
.menu-pop-leave-active { transition: opacity 0.12s; }
.menu-pop-enter-from { opacity: 0; transform: scale(0.9) translateY(-6px); }
.menu-pop-leave-to { opacity: 0; }
@media (prefers-reduced-motion: reduce) {
  .menu-pop-enter-active, .menu-pop-leave-active, .trigger, .item { transition: none; }
}
</style>
