import { computed, onBeforeUnmount, ref, watch, type Ref } from 'vue'

// Shared stack of currently-open full-screen overlays (modals, the terminal drawer) so that
// whichever one opened most recently owns Escape and renders on top, regardless of which
// component type it is. Without this, two independently-built overlays with their own window
// keydown listeners and hardcoded z-index would both react to one Escape press, and stacking
// order would depend on DOM insertion order instead of open order.
let nextId = 0
const stack = ref<number[]>([])

// Lock page scroll for as long as any overlay is open, and only unlock once the last one closes,
// so closing an inner overlay (e.g. the terminal drawer opened on top of a modal) can't unlock
// scrolling while another overlay is still up underneath it.
watch(stack, (ids) => { document.body.style.overflow = ids.length > 0 ? 'hidden' : '' })

/** Any overlay is open: views underneath (the decision cards) leave the keyboard to it. */
export const overlaysOpen = computed(() => stack.value.length > 0)

export function useOverlayLayer(open: Ref<boolean>) {
  const id = ++nextId
  const register = () => { if (!stack.value.includes(id)) stack.value = [...stack.value, id] }
  const unregister = () => { stack.value = stack.value.filter((x) => x !== id) }

  watch(open, (isOpen) => (isOpen ? register() : unregister()), { immediate: true })
  onBeforeUnmount(unregister)

  const isTop = computed(() => stack.value.length > 0 && stack.value[stack.value.length - 1] === id)
  const zIndex = computed(() => 1000 + Math.max(stack.value.indexOf(id), 0))
  return { isTop, zIndex }
}
