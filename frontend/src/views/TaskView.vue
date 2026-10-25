<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { useTaskStore } from '@/stores/tasks';
import { useConfigStore } from '@/stores/config';
import { useOperationStore } from '@/stores/operations';
import { useProjectStore } from '@/stores/projects';
import { jiraTicketUrl } from '@/api/client';
import WorkflowCanvas from '@/components/WorkflowCanvas.vue';
import AgentConfigurationDrawer from '@/components/AgentConfigurationDrawer.vue';
import RequirementsReview from '@/components/RequirementsReview.vue';
import QuestionModal from '@/components/QuestionModal.vue';
import FindingList from '@/components/FindingList.vue';
import FinalAssessment from '@/components/FinalAssessment.vue';
import EventLog from '@/components/EventLog.vue';
import ZellijViewer from '@/components/ZellijViewer.vue';
import WorktreeCleanup from '@/components/WorktreeCleanup.vue';
import PrTracking from '@/components/PrTracking.vue';
import ImplementerWorkers from '@/components/ImplementerWorkers.vue';
import ExecutionEvidence from '@/components/ExecutionEvidence.vue';
import TaskBudget from '@/components/TaskBudget.vue';
import RepositoryInstructions from '@/components/RepositoryInstructions.vue';
import DriftDiscard from '@/components/DriftDiscard.vue';
import DeleteTask from '@/components/DeleteTask.vue';
import MoveToProject from '@/components/MoveToProject.vue';
import Modal from '@/components/Modal.vue';
import ActivityFeed from '@/components/ActivityFeed.vue';
import ActionMenu, { type MenuItem } from '@/components/ActionMenu.vue';
import { usePersistedFlag } from '@/composables/usePersistedFlag';
import WorktreeSetup from '@/components/WorktreeSetup.vue';
import { DRIFT_DISCARDED_NOTICE, useWorktreeDrift } from '@/drift';
import { useOverlayLayer } from '@/overlayStack';
import { hasUsage, usageHeadline, useUsdToNzdRate } from '@/usage';
import { leftoverWorktree, phaseLabel, shortPath, statusTone } from '@/taskPresentation';
import { projectColor } from '@/projects';
import type { QuestionDto } from '@/api/types';

const props = defineProps<{ jiraKey: string }>();
const route = useRoute();
const router = useRouter();
const store = useTaskStore();
const operations = useOperationStore();
// A background cleanup or delete holds the task's lock; other task actions would only queue behind it.
const operationRunning = computed(() => !!operations.runningFor(props.jiraKey));
const configStore = useConfigStore();
const selected = ref<string | null>(null);
const tab = ref<'node' | 'requirements' | 'findings' | 'assessment' | 'events'>('node');
const notice = ref<string | null>(null);
const error = ref<string | null>(null);
const confirmingCancel = ref(false);
const confirmingDelete = ref(false);
const projects = useProjectStore();
const movingProject = ref(false);
const overrideReason = ref('');
const actionBusy = ref(false);
const showingInstructions = ref(false);

const SESSION_BADGE_CLASSES = new Set(['RUNNING', 'WAITING_FOR_USER', 'COMPLETED', 'FAILED', 'CANCELLED', 'SKIPPED', 'INTERRUPTED']);
const sessionBadgeClass = (activity: string): string => {
    const upper = activity.toUpperCase();
    return SESSION_BADGE_CLASSES.has(upper) ? upper : 'PENDING';
};

// Live classification of the pane's rendered screen (see docs/agent-state-tracking-plan.md),
// reusing the closest existing badge colors rather than inventing a new palette.
const SCREEN_STATE_BADGE_CLASSES: Record<string, string> = {
    working: 'RUNNING', blocked: 'WAITING_FOR_USER', idle: 'PENDING', unknown: 'SKIPPED',
};
const screenStateBadgeClass = (state: string): string => SCREEN_STATE_BADGE_CLASSES[state] ?? 'SKIPPED';

const task = computed(() => store.byKey(props.jiraKey));
const taskProject = computed(() => (task.value?.projectId == null ? null : projects.byId.get(task.value.projectId) ?? null));
const usdToNzdRate = useUsdToNzdRate();
const totalUsageLabel = computed(() => {
    const usage = task.value?.totalUsage;
    return hasUsage(usage) ? usageHeadline(usage, usdToNzdRate.value) : null;
});
const jiraUrl = computed(() => task.value?.taskSource === 'local'
    ? null : jiraTicketUrl(configStore.config?.jiraBaseUrl, props.jiraKey));
const selectedNode = computed(() => task.value?.nodes.find((node) => node.key === selected.value) ?? null);
// A status message the backend also logged as a warning (e.g. a base branch that couldn't be
// fetched from origin) reads as one, whatever the task's status.
const statusMessageTone = computed(() => {
    const current = task.value;
    if (!current) return 'info';
    if (current.events.some((e) => e.level === 'warning' && e.message === current.statusMessage)) return 'warn';
    if (['READY_FOR_PR', 'PR_CREATED', 'MERGED'].includes(current.status)) return 'ok';
    return current.status === 'NEEDS_HUMAN_REVIEW' ? 'warn' : 'info';
});
const openQuestions = computed(() => task.value?.questions.filter((question) => question.actionable) ?? []);
const terminalRoles = computed(() => {
    const sessions = task.value?.sessions.filter(
        (session) => session.status === 'running' && session.paneId,
    ) ?? [];
    return [...new Set(sessions.map((session) => session.nodeKey ?? session.role))];
});
const terminalLabel = (key: string): string =>
    task.value?.nodes.find((node) => node.key === key)?.label ?? key.replaceAll('_', ' ');
const terminalOpen = ref(false);
const { isTop: terminalIsTop, zIndex: terminalZIndex } = useOverlayLayer(terminalOpen);
const terminalRole = ref<string | null>(null);
const PANEL_MIN_WIDTH = 360;
const panelWidth = ref(0);
const resizing = ref(false);
const clampPanelWidth = (width: number): number => {
    const max = window.innerWidth * 0.92;
    return Math.min(Math.max(width, PANEL_MIN_WIDTH), Math.max(max, PANEL_MIN_WIDTH));
};
const onResizeMove = (event: PointerEvent): void => {
    if (!resizing.value) return;
    panelWidth.value = clampPanelWidth(window.innerWidth - event.clientX);
};
const stopResize = (): void => {
    resizing.value = false;
    window.removeEventListener('pointermove', onResizeMove);
    window.removeEventListener('pointerup', stopResize);
};
const startResize = (event: PointerEvent): void => {
    event.preventDefault();
    resizing.value = true;
    window.addEventListener('pointermove', onResizeMove);
    window.addEventListener('pointerup', stopResize);
};
const openTerminalPanel = (): void => {
    panelWidth.value = clampPanelWidth(window.innerWidth * 0.5);
    terminalOpen.value = true;
};
const onTerminalKeydown = (event: KeyboardEvent): void => {
    if (event.key === 'Escape' && terminalOpen.value && terminalIsTop.value) {
        terminalOpen.value = false;
    }
};
const attachCommandCopied = ref(false);
let attachCommandCopiedTimer: ReturnType<typeof setTimeout> | null = null;
const copyAttachCommand = async (): Promise<void> => {
    const command = task.value?.zellij.attachCommand;
    if (!command) return;
    try {
        await navigator.clipboard.writeText(command);
    } catch {
        return;
    }
    attachCommandCopied.value = true;
    if (attachCommandCopiedTimer) clearTimeout(attachCommandCopiedTimer);
    attachCommandCopiedTimer = setTimeout(() => { attachCommandCopied.value = false; }, 1500);
};

const drift = useWorktreeDrift(() => props.jiraKey, actionBusy, notice);

const overrideReviews = async (): Promise<void> => {
    actionBusy.value = true;
    drift.beginAction();
    try {
        await store.overrideReviews(props.jiraKey, overrideReason.value);
        overrideReason.value = '';
        drift.clearDrift();
    } catch (cause) {
        drift.noteFailure(cause);
    } finally {
        actionBusy.value = false;
    }
};

const load = async (): Promise<void> => {
    error.value = null;
    try {
        await store.loadTask(props.jiraKey);
        const key = route.query.node;
        if (typeof key === 'string' && task.value?.nodes.some((node) => node.key === key)) {
            selected.value = key;
            tab.value = key === 'requirements_gate' ? 'requirements'
                : key === 'ready_for_pr' ? 'assessment' : 'node';
        }
    } catch (cause) {
        error.value = (cause as Error).message;
    }
};

const onConfigure = (event: Event): void => {
    const key = (event as CustomEvent<string>).detail;
    selected.value = key;
    tab.value = key === 'requirements_gate' ? 'requirements' : key === 'ready_for_pr' ? 'assessment' : 'node';
};

const onOpenTerminal = (event: Event): void => {
    const detail = (event as CustomEvent<{ jiraKey: string; nodeKey?: string | null }>).detail;
    if (!detail || detail.jiraKey !== props.jiraKey) return;
    const current = task.value;
    const sessions = terminalRoles.value;
    const matchingNode = current?.nodes.find(
        (node) => node.key === detail.nodeKey || node.role === detail.nodeKey,
    );
    const waitingNode = current?.nodes.find((node) => node.state === 'WAITING_FOR_USER');
    const preferredRole = matchingNode?.key ?? waitingNode?.key ?? sessions[0] ?? null;
    terminalRole.value = preferredRole && sessions.includes(preferredRole)
        ? preferredRole
        : sessions[0] ?? null;
    openTerminalPanel();
};

const openSession = async (): Promise<void> => {
    notice.value = null;
    try {
        const result = await store.openZellij(props.jiraKey);
        notice.value = `${result.message}. Or attach manually: ${result.attachCommand}`;
    } catch (cause) {
        notice.value = (cause as Error).message;
    }
};

const terminalWindowError = ref<string | null>(null);
const openTerminalWindow = async (): Promise<void> => {
    terminalWindowError.value = null;
    try {
        await store.openZellijWindow(props.jiraKey, terminalRole.value);
    } catch (cause) {
        terminalWindowError.value = (cause as Error).message;
    }
};

const revealPath = async (target: 'worktree' | 'harnessCwd'): Promise<void> => {
    notice.value = null;
    try {
        await store.revealPath(props.jiraKey, target);
    } catch (cause) {
        notice.value = (cause as Error).message;
    }
};

const CANCELLABLE = new Set(['ACTIVE', 'NEEDS_HUMAN_REVIEW', 'READY_FOR_PR', 'PR_CREATED']);
// Nothing more will happen to the task: it may be archived or deleted.
const REMOVABLE = new Set(['MERGED', 'CANCELLED', 'FAILED']);
const cancellable = computed(() => !!task.value && CANCELLABLE.has(task.value.status));
const removable = computed(() => !!task.value && REMOVABLE.has(task.value.status));
// Past Ready for PR no agent is working, though persistent agent tabs may still be open.
const agentsWorking = computed(() => task.value?.status === 'ACTIVE' || task.value?.status === 'NEEDS_HUMAN_REVIEW');

const detailsOpen = usePersistedFlag('agentflow-task-details', true);

// The first thing waiting on the user, offered as the header's primary button. It goes to where the
// step is done (node, tab and camera) rather than doing it, since each one needs reading first.
const nextStep = computed(() => task.value?.attention?.[0] ?? null);
const leftover = computed(() => (task.value ? leftoverWorktree(task.value) : null));
const goToNextStep = (): void => {
    const key = nextStep.value?.nodeKey;
    if (!key) return;
    detailsOpen.value = true;
    selected.value = key;
    tab.value = key === 'requirements_gate' ? 'requirements' : key === 'ready_for_pr' ? 'assessment' : 'node';
    void router.replace({ query: { ...route.query, node: key } });
};

// Cancel is for live tasks; Archive and Delete only unlock once nothing more will happen.
const UNLOCK_HINT = 'Available after merge, cancel or failure';
const taskMenuItems = computed<MenuItem[]>(() => {
    if (!task.value) return [];
    const removeBlocked = !removable.value || operationRunning.value;
    return [
        { label: 'Move to project…', disabled: operationRunning.value, run: () => { movingProject.value = true; } },
        { label: 'Cancel task', danger: true, disabled: !cancellable.value, hint: cancellable.value ? undefined : 'Already finished',
          run: () => { confirmingCancel.value = true; } },
        task.value.archivedAt
            ? { label: 'Unarchive', disabled: actionBusy.value || operationRunning.value, run: () => void setArchived(false) }
            : { label: 'Archive', disabled: removeBlocked || actionBusy.value, hint: removable.value ? undefined : UNLOCK_HINT,
                run: () => void setArchived(true) },
        { label: 'Delete…', danger: true, disabled: removeBlocked, hint: removable.value ? undefined : UNLOCK_HINT,
          run: () => { confirmingDelete.value = true; } },
    ];
});

const setArchived = async (archived: boolean): Promise<void> => {
    actionBusy.value = true;
    notice.value = null;
    try {
        await (archived ? store.archiveTask(props.jiraKey) : store.unarchiveTask(props.jiraKey));
    } catch (cause) {
        notice.value = (cause as Error).message;
    } finally {
        actionBusy.value = false;
    }
};

// Deleted here or in another tab (the socket drops it from the store): nothing left to show.
// (Only for this key: switching to a task not yet loaded also passes through "no task".)
watch(task, (current, previous) => {
    if (previous && !current && previous.jiraKey === props.jiraKey) void router.replace({ name: 'home' });
});

// Shown inside the cancel dialog: the page's notice would sit behind its backdrop.
const cancelError = ref<string | null>(null);
watch(confirmingCancel, () => { cancelError.value = null; });
const cancel = async (): Promise<void> => {
    actionBusy.value = true;
    try {
        await store.cancelTask(props.jiraKey);
        confirmingCancel.value = false;
    } catch (cause) {
        cancelError.value = (cause as Error).message;
    } finally {
        actionBusy.value = false;
    }
};

const onQuestionAnswered = (question: QuestionDto): void => {
    const node = task.value?.nodes.find((n) => n.key === (question.nodeKey ?? question.role));
    const label = node?.label ?? question.role.replaceAll('_', ' ');
    notice.value = node?.state === 'RUNNING'
        ? `${label} resumed: every reply was sent.`
        : `Reply sent to ${label}.`;
};

// Focus the most useful panel when a workflow gate needs attention.
watch(task, (current, previous) => {
    if (!current || !previous || current.jiraKey !== previous.jiraKey) return;
    const gate = current.nodes.find((node) => node.key === 'requirements_gate');
    const previousGate = previous.nodes.find((node) => node.key === 'requirements_gate');
    if (gate?.state === 'WAITING_FOR_APPROVAL' && previousGate?.state !== 'WAITING_FOR_APPROVAL') {
        selected.value = 'requirements_gate';
        tab.value = 'requirements';
    }
    if (current.status === 'READY_FOR_PR' && previous.status !== 'READY_FOR_PR') {
        tab.value = 'assessment';
    }
});

onMounted(() => {
    void load();
    window.addEventListener('agentflow:configure', onConfigure);
    window.addEventListener('agentflow:open-terminal', onOpenTerminal);
    window.addEventListener('keydown', onTerminalKeydown);
});
onUnmounted(() => {
    window.removeEventListener('agentflow:configure', onConfigure);
    window.removeEventListener('agentflow:open-terminal', onOpenTerminal);
    window.removeEventListener('keydown', onTerminalKeydown);
    window.removeEventListener('pointermove', onResizeMove);
    window.removeEventListener('pointerup', stopResize);
    if (attachCommandCopiedTimer) clearTimeout(attachCommandCopiedTimer);
});
watch(() => props.jiraKey, () => void load());
watch(() => route.query.node, (key) => {
    if (typeof key !== 'string' || !task.value?.nodes.some((node) => node.key === key)) return;
    selected.value = key;
    tab.value = key === 'requirements_gate' ? 'requirements'
        : key === 'ready_for_pr' ? 'assessment' : 'node';
});
</script>

<template>
  <div v-if="error" class="alert error">{{ error }}</div>
  <div v-else-if="!task" class="muted">Loading task…</div>
  <template v-else>
   <div class="task-fold">
    <header class="task-head">
      <div class="task-head__text">
        <div class="task-head__title">
          <h1>
            <a v-if="jiraUrl" :href="jiraUrl" target="_blank" rel="noopener noreferrer">{{ task.jiraKey }}</a>
            <template v-else>{{ task.jiraKey }}</template>
            <template v-if="task.jiraTitle"> — {{ task.jiraTitle }}</template>
          </h1>
          <span class="status-pill" :class="nextStep ? 'attention' : statusTone(task.status)"><i class="pulse-dot" />{{ phaseLabel(task) }}</span>
          <span v-if="leftover" class="status-pill attention" :title="`Merged, but ${leftover} still on disk. Use Clean up worktree to remove it.`">Not cleaned up</span>
          <span v-if="task.archivedAt" class="status-pill">Archived</span>
          <button type="button" class="project-chip" :title="'Move to another project'" @click="movingProject = true">
            <i v-if="taskProject" class="project-dot" :style="{ '--dot': projectColor(taskProject.color) }" aria-hidden="true" />
            <i v-else class="project-dot none" aria-hidden="true" />
            {{ taskProject?.name ?? 'No project' }}
          </button>
        </div>
        <p class="task-head__meta muted">
          <span :title="task.repositoryRoot">{{ shortPath(task.repositoryRoot) }}<template v-if="task.projectRelativePath"> / {{ task.projectRelativePath }}</template></span>
          · <span class="mono branch">{{ task.branchName }}</span>
          · iteration {{ task.reviewIteration }} / {{ task.maxReviewIterations }}
          <template v-if="task.currentSha"> · <span class="mono">{{ task.currentSha.slice(0, 10) }}</span></template>
          <template v-if="totalUsageLabel"> · {{ totalUsageLabel }}</template>
        </p>
      </div>
      <div class="task-head__actions">
        <button @click="openSession">View Terminal</button>
        <button type="button" :class="{ active: detailsOpen }" :aria-pressed="detailsOpen" @click="detailsOpen = !detailsOpen">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="4" width="18" height="16" rx="4" /><path d="M15 4v16" /></svg>
          Details
        </button>
        <button v-if="nextStep" type="button" class="primary" @click="goToNextStep">{{ nextStep.action }}</button>
        <ActionMenu :items="taskMenuItems" label="More task actions" />
      </div>
    </header>
    <DeleteTask v-if="confirmingDelete" :task-key="task.jiraKey" @close="confirmingDelete = false"
                @deleted="router.replace({ name: 'home' })" />
    <MoveToProject v-if="movingProject" :task-key="task.jiraKey" :project-id="task.projectId" @close="movingProject = false" />
    <WorktreeSetup v-if="task.worktreeSetup?.length" :key="task.jiraKey" :task="task" />
    <div v-if="task.statusMessage" class="alert" :class="statusMessageTone">
      {{ task.statusMessage }}
    </div>
    <Modal v-if="confirmingCancel" title="Cancel this task?" narrow @close="confirmingCancel = false">
      <p v-if="agentsWorking" class="muted">Stop all agent tabs and cancel this task? Your branch, files and artifacts will be preserved.</p>
      <p v-else class="muted">Cancel this task and close any open agent tabs? Your branch, files, artifacts and any PR are left as they are.</p>
      <div v-if="cancelError" class="alert error small">{{ cancelError }}</div>
      <div class="row">
        <button class="danger solid" :disabled="actionBusy" @click="cancel">{{ agentsWorking ? 'Stop agents & cancel' : 'Cancel task' }}</button>
        <button :disabled="actionBusy" @click="confirmingCancel = false">Keep working</button>
      </div>
    </Modal>
    <WorktreeCleanup
      v-if="task.status === 'MERGED'"
      :key="task.jiraKey"
      :task-key="task.jiraKey"
      :expected-sha="task.currentSha"
      :worktree-exists="task.worktreeExists"
      :worker-worktree-count="task.workerWorktreeCount"
    />
    <form v-if="task.status === 'NEEDS_HUMAN_REVIEW'" class="panel" @submit.prevent="overrideReviews">
      <p>The review limit was reached. Run the implementer for another attempt, or override the review blockers to request a final assessment.</p>
      <ul><li v-for="blocker in task.blockers" :key="blocker">{{ blocker }}</li></ul>
      <label class="field"><span>Override reason</span><textarea v-model="overrideReason" maxlength="4000" required /></label>
      <button :disabled="actionBusy || !overrideReason.trim()">Override review blockers</button>
    </form>
    <Teleport to="body">
      <Transition name="terminal-slide">
        <div v-if="terminalOpen" class="terminal-overlay" :style="{ zIndex: terminalZIndex }">
          <div class="terminal-overlay__backdrop" @click="terminalOpen = false"></div>
          <aside class="terminal-drawer" :style="{ width: panelWidth + 'px' }">
            <div
              class="terminal-drawer__handle"
              :class="{ resizing }"
              @pointerdown="startResize"
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize terminal panel"
            ></div>
            <div class="panel-title">
              <div class="row">
                <h3 style="margin: 0">Agent terminal</h3>
                <label class="small">Pane
                  <select v-model="terminalRole">
                    <option v-for="role in terminalRoles" :key="role" :value="role">{{ terminalLabel(role) }}</option>
                  </select>
                </label>
              </div>
              <div class="row">
                <button
                  class="sm"
                  :disabled="!terminalRole"
                  v-if="task.zellij.attachCommand"
                  title="Open this agent's tab in an interactive terminal window"
                  @click="openTerminalWindow"
                >Open window</button>
                <button class="sm" @click="terminalOpen = false">Close</button>
              </div>
            </div>
            <p v-if="terminalWindowError" class="alert warn small">{{ terminalWindowError }}</p>
            <div class="terminal-drawer__body">
              <ZellijViewer v-if="terminalRole && terminalRoles.includes(terminalRole)" :key="`${task.jiraKey}:${terminalRole}`" :jira-key="task.jiraKey" :role="terminalRole" />
              <p v-else class="muted small">No active agent pane is available yet. Start an agent run to view its terminal here.</p>
            </div>
          </aside>
        </div>
      </Transition>
    </Teleport>
    <div v-if="drift.discarded.value" class="alert info">{{ DRIFT_DISCARDED_NOTICE }}</div>
    <div v-if="notice" class="alert info">
      {{ notice }}
      <DriftDiscard v-if="drift.worktreeDrift.value" v-model:confirming="drift.confirmingDiscard.value" :busy="actionBusy" @discard="drift.discardDrift" />
    </div>

    <div class="task-layout" :class="{ 'side-collapsed': !detailsOpen }">
      <WorkflowCanvas :task="task" :selected="selected" :locked="openQuestions.length > 0" :focus-key="typeof route.query.node === 'string' ? route.query.node : null" @select="(k) => { selected = k; if (k && tab !== 'node' && k !== 'requirements_gate' && k !== 'ready_for_pr') tab = 'node' }">
        <QuestionModal :key="task.jiraKey" :task="task" @answered="onQuestionAnswered" />
      </WorkflowCanvas>

      <aside class="side">
        <div class="panel">
          <div class="tabs">
            <button :class="{ active: tab === 'node' }" @click="tab = 'node'">Node</button>
            <button :class="{ active: tab === 'requirements' }" @click="tab = 'requirements'">Requirements</button>
            <button :class="{ active: tab === 'findings' }" @click="tab = 'findings'">Findings <span v-if="task.findings.length" class="badge">{{ task.findings.length }}</span></button>
            <button :class="{ active: tab === 'assessment' }" @click="tab = 'assessment'">Assessment</button>
            <button :class="{ active: tab === 'events' }" @click="tab = 'events'">Events</button>
          </div>

          <template v-if="tab === 'node'">
            <template v-if="selectedNode">
              <div class="panel-title">
                <h3 style="margin: 0">{{ selectedNode.label }}</h3>
                <span class="badge" :class="selectedNode.displayState">{{ selectedNode.displayState.replaceAll('_', ' ') }}</span>
              </div>
              <div v-if="selectedNode.error" class="alert error small" style="white-space: pre-wrap">{{ selectedNode.error }}</div>
              <AgentConfigurationDrawer v-if="selectedNode.kind === 'agent'" :node="selectedNode" :task-key="task.jiraKey" />
              <template v-else>
                <p class="muted small">{{ selectedNode.description }}</p>
                <p v-if="selectedNode.message" class="small">{{ selectedNode.message }}</p>
                <RequirementsReview v-if="selectedNode.key === 'requirements_gate'" :task="task" />
                <FinalAssessment v-if="selectedNode.key === 'ready_for_pr'" :task="task" />
              </template>
              <ImplementerWorkers v-if="selectedNode.key === 'implementer'" :task="task" />
            </template>
            <template v-else>
              <p class="muted small">Select a node in the graph to see its configuration, last run and actions.</p>
              <h4>Workspace</h4>
              <dl class="kv small">
                <dt>Repository</dt><dd class="mono">{{ task.repositoryRoot }}</dd>
                <dt>Working directory</dt><dd class="mono">{{ task.projectRelativePath ?? '(repository root)' }}</dd>
                <dt>Base branch</dt><dd class="mono">{{ task.baseBranch }}</dd>
                <dt>Branch</dt><dd class="mono">{{ task.branchName }}</dd>
                <dt>Worktree</dt>
                <dd class="mono">
                  <span class="copy-row">
                    <span>{{ task.worktreePath }}</span>
                    <button type="button" class="icon-btn" title="Open in file explorer" @click="revealPath('worktree')">
                      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path></svg>
                    </button>
                  </span>
                </dd>
                <dt>Agent CWD</dt>
                <dd class="mono">
                  <span class="copy-row">
                    <span>{{ task.harnessCwd }}</span>
                    <button type="button" class="icon-btn" title="Open in file explorer" @click="revealPath('harnessCwd')">
                      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path></svg>
                    </button>
                  </span>
                </dd>
                <dt>Task data</dt><dd class="mono">{{ task.taskDir }}</dd>
              <dt>Terminal</dt>
                <dd class="mono">
                  <span class="copy-row">
                    <span>{{ task.zellij.attachCommand }}</span>
                    <button type="button" class="icon-btn" :title="attachCommandCopied ? 'Copied!' : 'Copy command'" @click="copyAttachCommand">
                      <svg v-if="!attachCommandCopied" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"></rect><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path></svg>
                      <svg v-else width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"></polyline></svg>
                    </button>
                  </span>
                </dd>
                <dt v-if="task.zellij.webUrl">Web client</dt><dd v-if="task.zellij.webUrl"><a :href="task.zellij.webUrl" target="_blank">{{ task.zellij.webUrl }}</a></dd>
              </dl>
              <template v-if="task.additionalContext">
                <h4 style="margin-top: 0.8rem">Additional context</h4>
                <div class="md small">{{ task.additionalContext }}</div>
              </template>
              <template v-if="task.sessions.length">
                <h4 style="margin-top: 0.8rem">Agent sessions</h4>
                <table class="small">
                  <thead><tr><th>Role</th><th>Harness</th><th>Tab</th><th>Status</th><th>Screen</th></tr></thead>
                  <tbody>
                    <tr v-for="s in task.sessions" :key="s.id">
                      <td>{{ s.role.replace('_', ' ') }}</td><td>{{ s.harness }}</td><td>{{ s.tabName }}</td>
                      <td><span class="badge" :class="sessionBadgeClass(s.activity)">{{ s.activity.replaceAll('_', ' ') }}</span></td>
                      <td>
                        <span v-if="s.screenState" class="badge" :class="screenStateBadgeClass(s.screenState)" :title="s.screenStateAt ?? undefined">
                          {{ s.screenState }}
                        </span>
                        <span v-else class="muted">—</span>
                      </td>
                    </tr>
                  </tbody>
                </table>
              </template>
            </template>
          </template>

          <RequirementsReview v-else-if="tab === 'requirements'" :task="task" />
          <FindingList v-else-if="tab === 'findings'" :findings="task.findings" />
          <FinalAssessment v-else-if="tab === 'assessment'" :task="task" />
          <EventLog v-else :events="task.events" />
        </div>
      </aside>
    </div>
   </div>

    <div class="task-strip">
        <button @click="showingInstructions = true">Agent instructions</button>
        <RepositoryInstructions v-if="showingInstructions" :task="task"
            @close="showingInstructions = false" />
      <TaskBudget :task="task" />
      <ExecutionEvidence :task="task" />
      <PrTracking :key="task.jiraKey" :task="task" />
    </div>
    <ActivityFeed :events="task.events" />
  </template>
</template>

<style scoped>
.task-head { display: flex; flex-wrap: wrap; gap: 1rem 1.25rem; align-items: flex-end; justify-content: space-between; margin-bottom: 1rem; }
.task-head__text { display: flex; flex-direction: column; gap: 0.4rem; min-width: 0; }
.task-head__title { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem 1rem; }
/* The task's project; a click moves it to another. */
.project-chip {
  display: inline-flex; align-items: center; gap: 0.45rem; padding: 0.25rem 0.8rem; font-size: 12px; font-weight: 600;
  border: 1px solid var(--border); background: var(--panel); color: var(--text);
}
.task-head h1 { margin: 0; font-size: clamp(22px, 2.6vw, 30px); font-weight: 700; letter-spacing: -0.03em; line-height: 1.15; }
.task-head h1 a { color: var(--accent); }
.task-head .status-pill { white-space: normal; }
.task-head__meta { margin: 0; font-size: 14px; overflow-wrap: anywhere; }
.task-head__meta .mono { font-size: 13px; }
.task-head__meta .branch { color: var(--text); opacity: 0.85; }
.task-head__actions { display: flex; flex-wrap: wrap; align-items: center; gap: 0.6rem; }
.task-head__actions button { display: inline-flex; align-items: center; gap: 0.5rem; min-height: 44px; padding-inline: 1.2rem; }

.terminal-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    justify-content: flex-end;
}

.terminal-overlay__backdrop {
    position: absolute;
    inset: 0;
    background: rgb(0 0 0 / 45%);
    backdrop-filter: blur(4px);
    -webkit-backdrop-filter: blur(4px);
}

.terminal-drawer {
    position: relative;
    height: 100dvh;
    min-width: 360px;
    max-width: 92vw;
    background: var(--panel);
    border-left: 1px solid var(--border);
    box-shadow: -8px 0 24px rgb(0 0 0 / 25%);
    display: flex;
    flex-direction: column;
    padding: 1rem;
}

.terminal-drawer__handle {
    position: absolute;
    left: -3px;
    top: 0;
    bottom: 0;
    width: 6px;
    cursor: col-resize;
    touch-action: none;
    background: transparent;
}

.terminal-drawer__handle:hover,
.terminal-drawer__handle.resizing {
    background: var(--border);
}

.terminal-drawer__body {
    flex: 1;
    overflow: auto;
    margin-top: 0.5rem;
}

.terminal-drawer .panel-title {
    margin-bottom: 0.5rem;
}

.terminal-drawer select {
    width: auto;
    margin-left: 0.4rem;
}

.terminal-slide-enter-active .terminal-drawer,
.terminal-slide-leave-active .terminal-drawer {
    transition: transform 0.22s ease;
}

.terminal-slide-enter-active .terminal-overlay__backdrop,
.terminal-slide-leave-active .terminal-overlay__backdrop {
    transition: opacity 0.22s ease;
}

.terminal-slide-enter-from .terminal-drawer,
.terminal-slide-leave-to .terminal-drawer {
    transform: translateX(100%);
}

.terminal-slide-enter-from .terminal-overlay__backdrop,
.terminal-slide-leave-to .terminal-overlay__backdrop {
    opacity: 0;
}

@media (prefers-reduced-motion: reduce) {
    .terminal-slide-enter-active .terminal-drawer,
    .terminal-slide-leave-active .terminal-drawer,
    .terminal-slide-enter-active .terminal-overlay__backdrop,
    .terminal-slide-leave-active .terminal-overlay__backdrop {
        transition: none;
    }
}

.copy-row {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
}

.icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0.15rem;
    border: 1px solid transparent;
    background: none;
    color: var(--muted);
    cursor: pointer;
    line-height: 0;
    border-radius: 4px;
}

.icon-btn:hover {
    color: var(--text);
    border-color: var(--border-strong);
    background: var(--hover);
}
</style>
