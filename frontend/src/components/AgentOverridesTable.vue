<script lang="ts">
import type { AgentOverride } from '@/api/types'

export const OVERRIDE_ROLES: { key: string; label: string; configKey: string }[] = [
  { key: 'architect', label: 'Architect', configKey: 'architect' },
  // Set only for a separate planner; otherwise the Architect plans Parallel tasks.
  { key: 'orchestrator', label: 'Orchestrator (separate planner)', configKey: 'orchestrator' },
  { key: 'implementer', label: 'Implementer', configKey: 'implementer' },
  { key: 'code_reviewer', label: 'Code Reviewer', configKey: 'codeReviewer' },
  { key: 'security_reviewer', label: 'Security Reviewer', configKey: 'securityReviewer' },
]

/** One empty override per role, as the table binds every role's fields. */
export const emptyOverrides = (): Record<string, AgentOverride> =>
  Object.fromEntries(OVERRIDE_ROLES.map((role) => [role.key, {}]))

/** Only the fields that are set, and only the roles that have any (what the backend stores). */
export function cleanOverrides(overrides: Record<string, AgentOverride>): Record<string, AgentOverride> {
  const result: Record<string, AgentOverride> = {}
  for (const [role, o] of Object.entries(overrides)) {
    const clean: AgentOverride = {}
    if (o.harness) clean.harness = o.harness
    if (o.model) clean.model = o.model
    if (o.reasoning) clean.reasoning = o.reasoning
    if (Object.keys(clean).length) result[role] = clean
  }
  return result
}
</script>

<script setup lang="ts">
import { useConfigStore } from '@/stores/config'

// Every role's fields are edited in place, so the object must hold all of OVERRIDE_ROLES.
const overrides = defineModel<Record<string, AgentOverride>>({ required: true })
const configStore = useConfigStore()
</script>

<template>
  <table>
    <thead><tr><th>Role</th><th>Harness</th><th>Model</th><th>Reasoning</th></tr></thead>
    <tbody>
      <tr v-for="role in OVERRIDE_ROLES" :key="role.key">
        <td>{{ role.label }}<div class="muted small">default: {{ configStore.config?.agents[role.configKey]?.harness }}</div></td>
        <td>
          <select v-model="overrides[role.key].harness" :aria-label="`${role.label} harness`">
            <option :value="undefined">(default)</option>
            <option v-for="h in configStore.harnesses" :key="h.key" :value="h.key" :disabled="!h.available || !h.enabled">
              {{ h.displayName }}{{ h.available && h.enabled ? '' : ' — unavailable' }}
            </option>
          </select>
        </td>
        <td><input type="text" v-model="overrides[role.key].model" placeholder="harness default" :aria-label="`${role.label} model`" /></td>
        <td>
          <select v-model="overrides[role.key].reasoning" :aria-label="`${role.label} reasoning`">
            <option :value="undefined">(default)</option>
            <option value="low">low</option><option value="medium">medium</option><option value="high">high</option>
          </select>
        </td>
      </tr>
    </tbody>
  </table>
</template>
