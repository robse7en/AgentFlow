<script setup lang="ts">
defineProps<{ count: number }>()
</script>

<template>
  <div class="input-required">
    ⚠ INPUT REQUIRED
    <div class="small" style="font-weight: 500">
      {{ count }} unanswered question{{ count === 1 ? '' : 's' }} — reply in AgentFlow
    </div>
  </div>
</template>
