<script setup lang="ts">
// Smoothstep edge whose horizontal run is pinned to a fixed height (LOOPBACK_Y) instead of
// vue-flow's automatic midpoint. Used for the consolidate->implementer loop-back edge so it
// arcs above Code Review rather than looping tightly between the two same-height nodes. Its
// baton (amber, EdgeBaton.vue) runs the same arc.
import { computed } from 'vue'
import { BaseEdge, getSmoothStepPath, type EdgeProps } from '@vue-flow/core'
import { LOOPBACK_Y } from '@/workflow/layout'
import type { Baton } from '@/workflow/batons'
import EdgeBaton from './EdgeBaton.vue'

const props = defineProps<EdgeProps<{ baton?: Baton | null }>>()

const pathData = computed(() =>
  getSmoothStepPath({
    sourceX: props.sourceX,
    sourceY: props.sourceY,
    sourcePosition: props.sourcePosition,
    targetX: props.targetX,
    targetY: props.targetY,
    targetPosition: props.targetPosition,
    borderRadius: 8,
    centerY: LOOPBACK_Y,
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
