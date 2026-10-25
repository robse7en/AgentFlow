<script setup lang="ts">
// The "Discard drift…" button and its irreversible-action confirmation, shared by the
// approval node and the review-override panel (see src/drift.ts for the state behind it).
defineProps<{ busy: boolean }>();
const confirming = defineModel<boolean>('confirming', { required: true });
const emit = defineEmits<{ discard: [] }>();
</script>

<template>
  <div v-if="!confirming">
    <button class="sm" :disabled="busy" @click="confirming = true">Discard drift…</button>
  </div>
  <div v-else>
    <p>This permanently discards the uncommitted changes listed above; it cannot be undone. Only do this if none of them are real edits you want to keep.</p>
    <button class="sm danger" :disabled="busy" @click="emit('discard')">Yes, discard and match the checkpoint</button>
    <button class="sm" :disabled="busy" @click="confirming = false">Cancel</button>
  </div>
</template>
