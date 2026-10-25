import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { api } from '@/api/client'
import type { Project, ProjectPayload } from '@/api/types'

export const useProjectStore = defineStore('projects', () => {
  /** Every project, archived ones included, by name (as the server sends them). */
  const projects = ref<Project[]>([])
  const loaded = ref(false)
  const error = ref<string | null>(null)

  async function load() {
    try {
      apply(await api.projects())
      error.value = null
    } catch (e) {
      error.value = (e as Error).message
    }
  }

  /** The whole list, from a load or a `projects` event (every change pushes the list). */
  function apply(list: Project[]) {
    projects.value = list
    loaded.value = true
  }

  // Each change also arrives as an event; applying the response as well keeps this tab current if
  // the socket is down.
  function upsert(project: Project) {
    const rest = projects.value.filter((p) => p.id !== project.id)
    projects.value = [...rest, project].sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()))
    return project
  }

  const create = async (payload: ProjectPayload) => upsert(await api.createProject(payload))
  const update = async (id: number, payload: ProjectPayload) => upsert(await api.updateProject(id, payload))
  const archive = async (id: number) => upsert(await api.archiveProject(id))
  const unarchive = async (id: number) => upsert(await api.unarchiveProject(id))
  async function remove(id: number) {
    const result = await api.deleteProject(id)
    projects.value = projects.value.filter((p) => p.id !== id)
    return result
  }

  const byId = computed(() => new Map(projects.value.map((p) => [p.id, p])))
  const bySlug = computed(() => new Map(projects.value.map((p) => [p.slug, p])))
  /** The ones a task can be put in; archived projects keep their tasks but take no new ones. */
  const active = computed(() => projects.value.filter((p) => !p.archivedAt))
  const archived = computed(() => projects.value.filter((p) => p.archivedAt))

  return { projects, loaded, error, byId, bySlug, active, archived, load, apply, create, update, archive, unarchive, remove }
})
