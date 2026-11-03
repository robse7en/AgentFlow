<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { useOverlayLayer } from '@/overlayStack'

// `narrow`: a short confirmation (a question and its buttons) rather than a form or a list.
const props = defineProps<{ title: string; narrow?: boolean }>()
const emit = defineEmits<{ close: [] }>()

// This component's whole lifetime is "open" (the parent mounts it via v-if), so it registers
// itself with the shared overlay stack unconditionally.
const { isTop, zIndex } = useOverlayLayer(ref(true))
// For content with its own keys (e.g. arrow-key paging): act only while this dialog is on top.
defineExpose({ isTop })

const dialogRef = ref<HTMLElement | null>(null)
let previouslyFocused: HTMLElement | null = null

function focusableElements(): HTMLElement[] {
  if (!dialogRef.value) return []
  return Array.from(
    dialogRef.value.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), '
        + 'select:not([disabled]), [tabindex]:not([tabindex="-1"])',
    ),
  ).filter((el) => el.offsetParent !== null)
}

function onKeydown(e: KeyboardEvent) {
  if (!isTop.value) return
  if (e.key === 'Escape') {
    emit('close')
    return
  }
  if (e.key !== 'Tab') return
  const focusable = focusableElements()
  if (!focusable.length) {
    e.preventDefault()
    return
  }
  const first = focusable[0]
  const last = focusable[focusable.length - 1]
  if (e.shiftKey && document.activeElement === first) {
    e.preventDefault()
    last.focus()
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault()
    first.focus()
  }
}

onMounted(() => {
  previouslyFocused = document.activeElement as HTMLElement | null
  window.addEventListener('keydown', onKeydown)
  void nextTick(() => {
    (focusableElements()[0] ?? dialogRef.value)?.focus()
  })
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
  if (previouslyFocused?.isConnected) previouslyFocused.focus()
})
</script>

<template>
  <Teleport to="body">
    <Transition name="modal-fade" appear>
      <div class="modal-overlay" :style="{ zIndex }">
        <div class="modal-overlay__backdrop" @click="emit('close')"></div>
        <div ref="dialogRef" class="modal-dialog panel" :class="{ narrow: props.narrow }" role="dialog" aria-modal="true"
            :aria-label="props.title" tabindex="-1">
          <div class="panel-title">
            <h3 style="margin: 0">{{ props.title }}</h3>
            <div class="modal-dialog__actions">
              <slot name="actions" />
              <button class="sm" @click="emit('close')">Close</button>
            </div>
          </div>
          <div class="modal-dialog__body">
            <slot />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.modal-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1.5rem;
}

.modal-overlay__backdrop {
    position: absolute;
    inset: 0;
    background: rgb(5 6 10 / 60%);
    backdrop-filter: blur(6px);
    -webkit-backdrop-filter: blur(6px);
}

.modal-dialog {
    position: relative;
    width: min(1080px, 100%);
    max-height: min(80vh, 910px);
    margin-bottom: 0;
    display: flex;
    flex-direction: column;
    padding: 1.5rem 1.6rem;
    border-radius: 28px;
    box-shadow: 0 30px 80px var(--shadow-1);
}

.modal-dialog.narrow { width: min(460px, 100%); }
.modal-dialog h3 { font-size: 1.2rem; letter-spacing: -0.01em; min-width: 0; overflow-wrap: anywhere; }
.modal-dialog .panel-title { gap: 1rem; align-items: flex-start; }
.modal-dialog__actions { display: flex; align-items: center; gap: 0.4rem; flex: none; }

.modal-dialog__body {
    overflow: auto;
}

.modal-fade-enter-active .modal-dialog {
    transition: transform 0.35s var(--spring), opacity 0.2s ease;
}

.modal-fade-leave-active .modal-dialog {
    transition: transform 0.18s ease, opacity 0.18s ease;
}

.modal-fade-enter-active .modal-overlay__backdrop,
.modal-fade-leave-active .modal-overlay__backdrop {
    transition: opacity 0.18s ease;
}

.modal-fade-enter-from .modal-dialog,
.modal-fade-leave-to .modal-dialog {
    opacity: 0;
    transform: scale(0.92) translateY(-6px);
}

.modal-fade-enter-from .modal-overlay__backdrop,
.modal-fade-leave-to .modal-overlay__backdrop {
    opacity: 0;
}

@media (prefers-reduced-motion: reduce) {
    .modal-fade-enter-active .modal-dialog,
    .modal-fade-leave-active .modal-dialog,
    .modal-fade-enter-active .modal-overlay__backdrop,
    .modal-fade-leave-active .modal-overlay__backdrop {
        transition: none;
    }
}
</style>
