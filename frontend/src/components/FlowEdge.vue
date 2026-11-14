<script setup lang="ts">
// Every forward edge: vue-flow's bezier edge as it was, plus the baton that runs along it at a handoff
// (EdgeBaton.vue). The edge's `data.baton` is set by WorkflowCanvas for the length of the baton's life.
import { computed } from 'vue'
import { BaseEdge, getBezierPath, type EdgeProps } from '@vue-flow/core'
import type { Baton } from '@/workflow/batons'
import EdgeBaton from './EdgeBaton.vue'

const props = defineProps<EdgeProps<{ baton?: Baton | null }>>()

const pathData = computed(() =>
  getBezierPath({
    sourceX: props.sourceX,
    sourceY: props.sourceY,
    sourcePosition: props.sourcePosition,
    targetX: props.targetX,
    targetY: props.targetY,
    targetPosition: props.targetPosition,
  }),
)
</script>

<template>
  <BaseEdge
    :id="id"
    :path="pathData[0]"
    :label-x="pathData[1]"
    :label-y="pathData[2]"
    :marker-start="markerStart"
    :marker-end="markerEnd"
    :style="style"
    :label="label"
    :label-style="labelStyle"
    :label-bg-style="labelBgStyle"
  />
  <EdgeBaton :path="pathData[0]" :baton="data?.baton ?? null" />
</template>
