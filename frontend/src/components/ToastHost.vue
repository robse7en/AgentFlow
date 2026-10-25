<script setup lang="ts">
import { useToastStore, type Toast, type ToastAction } from '@/stores/toasts'

const store = useToastStore()

const act = (toast: Toast, action: ToastAction) => {
    store.dismiss(toast.id)
    action.run()
}
</script>

<template>
    <div class="toasts" aria-live="polite">
        <TransitionGroup name="toast">
            <div v-for="toast in store.toasts" :key="toast.id" class="toast" :class="toast.kind"
                 :role="toast.kind === 'error' ? 'alert' : 'status'">
                <span class="icon" aria-hidden="true">
                    <span v-if="toast.kind === 'progress'" class="spinner" />
                    <template v-else-if="toast.kind === 'success'">✓</template>
                    <template v-else>!</template>
                </span>
                <div class="body">
                    <strong>{{ toast.title }}</strong>
                    <p v-if="toast.message" class="small">{{ toast.message }}</p>
                    <div v-if="toast.actions.length" class="actions">
                        <button v-for="action in toast.actions" :key="action.label" type="button" class="sm"
                                @click="act(toast, action)">{{ action.label }}</button>
                    </div>
                </div>
                <button v-if="toast.kind !== 'progress'" type="button" class="close" title="Dismiss"
                        aria-label="Dismiss" @click="store.dismiss(toast.id)">×</button>
            </div>
        </TransitionGroup>
    </div>
</template>

<style scoped>
.toasts {
    position: fixed; right: 1rem; bottom: 1rem; z-index: 1100;
    display: flex; flex-direction: column; gap: 0.5rem;
    width: min(24rem, calc(100vw - 2rem)); pointer-events: none;
}
.toast {
    pointer-events: auto; display: flex; gap: 0.6rem; align-items: flex-start;
    padding: 0.65rem 0.75rem; border-radius: 8px; border: 1px solid var(--border);
    border-left: 4px solid var(--run); background: var(--panel); color: var(--text); box-shadow: var(--elev);
}
.toast.success { border-left-color: var(--ok); }
.toast.warning { border-left-color: var(--warn); }
.toast.error { border-left-color: var(--danger); }
.icon { flex: none; width: 1.1rem; text-align: center; font-weight: 700; line-height: 1.3; }
.success .icon { color: var(--ok); }
.warning .icon { color: var(--warn); }
.error .icon { color: var(--danger); }
.body { flex: 1; min-width: 0; }
.body p { margin: 0.2rem 0 0; color: var(--muted); overflow-wrap: anywhere; }
.actions { display: flex; gap: 0.4rem; margin-top: 0.5rem; }
.close { flex: none; border: none; background: none; padding: 0 0.2rem; font-size: 1.1rem; line-height: 1; color: var(--muted); }
.spinner {
    display: inline-block; width: 0.85rem; height: 0.85rem; margin-top: 0.15rem;
    border: 2px solid var(--run-soft); border-top-color: var(--run); border-radius: 50%;
    animation: spin 0.8s linear infinite;
}
@keyframes spin { to { transform: rotate(360deg); } }
.toast-enter-active, .toast-leave-active { transition: opacity 0.2s, transform 0.2s; }
.toast-enter-from, .toast-leave-to { opacity: 0; transform: translateY(0.5rem); }
@media (prefers-reduced-motion: reduce) {
    .spinner { animation-duration: 2.4s; }
    .toast-enter-active, .toast-leave-active { transition: none; }
}
</style>
