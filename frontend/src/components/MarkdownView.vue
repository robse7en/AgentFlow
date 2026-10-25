<script setup lang="ts">
import { computed } from 'vue'
import { marked } from 'marked'
import DOMPurify from 'dompurify'

const props = defineProps<{ source: string | null | undefined }>()

marked.setOptions({ breaks: true, gfm: true })

const html = computed(() => {
  const raw = props.source?.trim()
  if (!raw) return ''
  return DOMPurify.sanitize(marked.parse(raw, { async: false }) as string)
})
</script>

<template>
  <div class="md" v-if="html" v-html="html"></div>
  <div class="md muted" v-else>—</div>
</template>
