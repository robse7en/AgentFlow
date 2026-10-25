<script setup lang="ts">
import { Handle, Position } from '@vue-flow/core'
import type { WorkflowNodeDto } from '@/api/types'
import AgentNode from './AgentNode.vue'
import ApprovalNode from './ApprovalNode.vue'
import NodeFooter from './NodeFooter.vue'

defineProps<{
  data: { node: WorkflowNodeDto; taskKey: string; selected: boolean }
}>()
</script>

<template>
  <div class="wf-node" :class="[data.node.displayState, { selected: data.selected }]">
    <Handle type="target" :position="Position.Left" />
    <div class="wf-body">
      <AgentNode v-if="data.node.kind === 'agent'" :node="data.node" :task-key="data.taskKey" />
      <ApprovalNode v-else :node="data.node" :task-key="data.taskKey" />
    </div>
    <!-- Gates have no runs to time or cost unless they ran something (a system step). -->
    <NodeFooter v-if="data.node.kind === 'agent' || data.node.lastRun" :node="data.node" />
    <Handle type="source" :position="Position.Right" />
  </div>
</template>
