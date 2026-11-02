<script setup lang="ts">
import { computed } from 'vue'
import type { TaskSnapshot } from '@/api/types'
import MarkdownView from '@/components/MarkdownView.vue'

const props = defineProps<{ task: TaskSnapshot }>()

const done = (key: string) => props.task.nodes.find((n) => n.key === key)?.state === 'COMPLETED'
const reviewPassed = (key: string, type: string) => {
    const node = props.task.nodes.find((candidate) => candidate.key === key);
    return node?.state === 'COMPLETED'
        && node.checkpointSha === props.task.currentSha
        && !props.task.findings.some((finding) => finding.type === type
            && finding.iteration === props.task.reviewIteration && finding.blocking);
};
const list = (value: unknown): string[] => (Array.isArray(value) ? (value as string[]) : [])
const verified = computed(() => {
  const value = props.task.assessment?.requirementsVerified
  if (value == null) return '—'
  return value ? 'yes' : 'NO'
})
const riskClass = computed(() => {
  const risk = props.task.assessment?.overallRisk
  return risk === 'LOW' ? 'ok' : risk === 'HIGH' ? 'error' : 'warn'
})
</script>

<template>
  <div>
    <div v-if="task.status === 'READY_FOR_PR'" class="alert info" style="margin-bottom: 0.9rem">
      Ready for PR. Use <b>Push &amp; open PR</b> at the top of the page to push <code>{{ task.branchName }}</code>
      and open GitHub's pull request form into <code>{{ task.baseBranch }}</code>.
    </div>
    <ul class="check-list" style="margin-bottom: 0.9rem">
      <li :class="done('requirements_gate') ? 'ok' : 'bad'">Requirements approved</li>
      <li :class="done('implementer') && task.currentSha ? 'ok' : 'bad'">
        Implementation completed <span v-if="task.currentSha" class="mono muted small">{{ task.currentSha.slice(0, 10) }}</span>
      </li>
      <li :class="reviewPassed('code_review', 'code-review') ? 'ok' : 'bad'">Code Review passed</li>
      <li :class="reviewPassed('security_review', 'security-review') ? 'ok' : 'bad'">Security Review passed</li>
      <li :class="task.assessment?.requirementsVerified === true ? 'ok' : 'bad'">
        Requirements verified by Architect
      </li>
    </ul>
    <div v-if="task.blockers.length" class="alert warn">
      <b>Readiness blockers</b>
      <ul><li v-for="blocker in task.blockers" :key="blocker">{{ blocker }}</li></ul>
      <p>A human override preserves this evidence and records a reason against this commit.</p>
    </div>

    <template v-if="task.assessment">
      <div class="row" style="margin-bottom: 0.75rem">
        <span>Risk:</span>
        <span class="badge" :class="task.assessment.overallRisk">{{ task.assessment.overallRisk }}</span>
        <span class="muted">
          Complexity: <b>{{ task.assessment.complexity ?? '—' }}</b>
          <span v-if="task.assessment.complexityScore"> ({{ task.assessment.complexityScore }}/10)</span>
        </span>
      </div>
      <div class="alert" :class="riskClass"><b>Recommendation:</b> {{ task.assessment.humanReviewRecommendation }}</div>
      <dl class="kv small">
        <dt>Summary</dt><dd>{{ task.assessment.taskSummary }}</dd>
        <dt>Regression risk</dt><dd>{{ task.assessment.regressionRisk ?? '—' }}</dd>
        <dt>Security risk</dt><dd>{{ task.assessment.securityRisk ?? '—' }}</dd>
        <dt>Requirements verified</dt><dd>{{ verified }}</dd>
        <dt>Iterations</dt><dd>{{ task.assessment.reviewIterations }} / {{ task.maxReviewIterations }}</dd>
        <dt>Final SHA</dt><dd class="mono">{{ task.assessment.finalSha }}</dd>
        <dt>Tests</dt><dd>{{ task.assessment.testsPerformed ?? '—' }}</dd>
      </dl>
      <template v-if="list(task.assessment.riskFactors).length">
        <h4 style="margin-top: 0.8rem">Risk factors</h4>
        <ul class="small"><li v-for="r in list(task.assessment.riskFactors)" :key="r">{{ r }}</li></ul>
      </template>
      <template v-if="list(task.assessment.knownLimitations).length">
        <h4 style="margin-top: 0.8rem">Known limitations</h4>
        <ul class="small"><li v-for="r in list(task.assessment.knownLimitations)" :key="r">{{ r }}</li></ul>
      </template>
      <template v-if="list(task.assessment.outstandingUncertainty).length">
        <h4 style="margin-top: 0.8rem">Outstanding uncertainty</h4>
        <ul class="small"><li v-for="r in list(task.assessment.outstandingUncertainty)" :key="r">{{ r }}</li></ul>
      </template>
      <details style="margin-top: 0.75rem" v-if="task.assessmentMarkdown">
        <summary class="small">Full assessment (assessment.md)</summary>
        <MarkdownView :source="task.assessmentMarkdown" />
      </details>
    </template>
    <p v-else class="muted small">The final assessment appears here once the Architect completes final verification.</p>
  </div>
</template>
