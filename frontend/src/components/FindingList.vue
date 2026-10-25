<script setup lang="ts">
import { computed } from 'vue'
import type { Finding } from '@/api/types'

const props = defineProps<{ findings: Finding[] }>()

const grouped = computed(() => {
  const map = new Map<number, Finding[]>()
  for (const f of props.findings) {
    const bucket = map.get(f.iteration) ?? []
    bucket.push(f)
    map.set(f.iteration, bucket)
  }
  return [...map.entries()].sort((a, b) => b[0] - a[0])
})
const blocking = (items: Finding[]) => items.filter((f) => f.blocking).length
</script>

<template>
  <div>
    <p v-if="!findings.length" class="muted small">No review findings recorded yet.</p>
    <div v-for="[iteration, items] in grouped" :key="iteration" style="margin-bottom: 1rem">
      <h4>
        Iteration {{ iteration }}
        <span class="muted" style="text-transform: none; letter-spacing: 0">
          — {{ items.length }} finding{{ items.length === 1 ? '' : 's' }}, {{ blocking(items) }} blocking, SHA {{ items[0].reviewedSha?.slice(0, 10) }}
        </span>
      </h4>
      <div v-for="f in items" :key="f.dbId" class="finding" :class="f.status">
        <div class="head">
          <b class="mono">{{ f.id }}</b>
          <span class="badge" :class="f.severity">{{ f.severity }}</span>
          <span v-if="f.blocking" class="badge FAILED">blocking</span>
          <span v-if="f.occurrence > 1" class="badge WAITING_FOR_USER">raised {{ f.occurrence }}×</span>
          <span class="badge" :class="f.status === 'open' ? 'WAITING_FOR_USER' : 'COMPLETED'">{{ f.status }}</span>
          <span class="muted small">{{ f.type }}</span>
        </div>
        <div style="margin-top: 0.25rem"><b>{{ f.title }}</b></div>
        <div v-if="f.file" class="loc">{{ f.file }}<span v-if="f.line">:{{ f.line }}</span></div>
        <div v-if="f.description" class="small" style="margin-top: 0.25rem; white-space: pre-wrap">{{ f.description }}</div>
        <div v-if="f.recommendation" class="small muted" style="margin-top: 0.25rem; white-space: pre-wrap">
          <b>Recommendation:</b> {{ f.recommendation }}
        </div>
        <div v-if="f.response" class="small" style="margin-top: 0.25rem; white-space: pre-wrap">
          <b>Implementer {{ f.response }}:</b> {{ f.responseNote }}
        </div>
        <div v-if="f.resolution" class="small" style="margin-top: 0.25rem; white-space: pre-wrap">
          <b>Reviewer evidence:</b> {{ f.resolution }}
        </div>
      </div>
    </div>
  </div>
</template>
