import { ref } from 'vue'
import type { AgentOverride } from './api/types'

/** Everything the New Task form holds, so a task that failed to create in the background can be edited and retried. */
export interface TaskDraft {
  taskSource: 'local' | 'jira'
  prompt: string
  implementationMode: 'direct' | 'parallel' | ''
  form: {
    jiraKey: string
    jiraTitle: string
    repositoryPath: string
    projectRelativePath: string
    baseBranch: string
    additionalContext: string
  }
  overrides: Record<string, AgentOverride>
  projectId?: number | null
  /** Why the last attempt failed, shown on the restored form. */
  error?: string
}

/** The draft waiting for the form to pick it up (the form may already be open). */
export const pendingDraft = ref<TaskDraft | null>(null)
