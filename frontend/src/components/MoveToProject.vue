<script setup lang="ts">
import { computed, ref } from 'vue'
import { useTaskStore } from '@/stores/tasks'
import { useProjectStore } from '@/stores/projects'
import { projectColor } from '@/projects'
import Modal from './Modal.vue'

const props = defineProps<{ taskKey: string; projectId: number | null }>()
const emit = defineEmits<{ close: [] }>()

const tasks = useTaskStore()
const projects = useProjectStore()
const chosen = ref<number | null>(props.projectId)
const busy = ref(false)
const error = ref<string | null>(null)

// Archived projects take no new tasks, but the task's own one stays listed so the select can show it.
const options = computed(() => projects.projects.filter((p) => !p.archivedAt || p.id === props.projectId))
const selected = computed(() => (chosen.value == null ? null : projects.byId.get(chosen.value) ?? null))

async function save() {
  if (chosen.value === props.projectId) return emit('close')
  busy.value = true
  error.value = null
  try {
    await tasks.setTaskProject(props.taskKey, chosen.value)
    emit('close')
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Could not move the task.'
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <Modal title="Move to project" narrow @close="emit('close')">
    <p class="small muted">{{ taskKey }}</p>
    <form @submit.prevent="save">
      <label class="field">
        <span>Project</span>
        <div class="input-row move-select">
          <span class="project-dot" :class="{ none: !selected }" :style="selected ? { '--dot': projectColor(selected.color) } : undefined" aria-hidden="true" />
          <select v-model="chosen" :disabled="busy">
            <option :value="null">No project</option>
            <option v-for="p in options" :key="p.id" :value="p.id">{{ p.name }}{{ p.archivedAt ? ' (archived)' : '' }}</option>
          </select>
        </div>
      </label>
      <div v-if="error" class="alert error small">{{ error }}</div>
      <div class="row" style="justify-content: flex-end">
        <button type="button" class="sm" :disabled="busy" @click="emit('close')">Cancel</button>
        <button type="submit" class="sm primary" :disabled="busy">Move</button>
      </div>
    </form>
  </Modal>
</template>

<style scoped>
.move-select { align-items: center; }
.move-select .project-dot { width: 11px; height: 11px; margin-inline: 0.2rem; }
.move-select select { flex: 1; }
</style>
