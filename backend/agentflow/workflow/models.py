"""Pydantic contracts: structured agent results and API payloads."""

from __future__ import annotations

from typing import Annotated, Any, Literal
from uuid import UUID

from pydantic import BaseModel, ConfigDict, Field, StrictInt, field_validator, model_validator

from . import decisions
from .projects import PROJECT_COLORS

Severity = Literal["info", "low", "medium", "high", "critical"]
SEVERITY_ORDER = {"info": 0, "low": 1, "medium": 2, "high": 3, "critical": 4}


class Contract(BaseModel):
    model_config = ConfigDict(populate_by_name=True, extra="allow")


# --------------------------------------------------------------------------------------
# Agent → AgentFlow contracts
# --------------------------------------------------------------------------------------
class Finding(Contract):
    id: str | None = None
    type: str | None = None
    severity: Severity = "medium"
    blocking: bool | None = None
    file: str | None = None
    line: int | None = None
    title: str = ""
    description: str | None = None
    recommendation: str | None = None
    prior_id: str | None = Field(None, alias="priorId")  # a re-raised finding from the reviewer's previous round

    @field_validator("severity", mode="before")
    @classmethod
    def _norm_severity(cls, value: Any) -> Any:
        if isinstance(value, str):
            value = value.strip().lower()
            aliases = {"informational": "info", "warning": "low", "moderate": "medium", "major": "high", "blocker": "critical"}
            return aliases.get(value, value)
        return value

    def is_blocking(self) -> bool:
        if self.blocking is not None:
            return self.blocking
        return SEVERITY_ORDER[self.severity] >= SEVERITY_ORDER["medium"]


class TestSummary(Contract):
    executed: int | None = Field(None, ge=0)
    passed: int | None = Field(None, ge=0)
    failed: int | None = Field(None, ge=0)
    notes: str | None = None

    @model_validator(mode="after")
    def validate_counts(self):
        if self.executed is not None and (self.passed or 0) + (self.failed or 0) > self.executed:
            raise ValueError("Passed and failed counts exceed executed tests")
        return self


# The engine checks these before accepting a result (and answers a bad one with a resubmittable
# 400), so the fields here stay plain strings: a typo must never fail the whole run.
FINDING_RESPONSE_ACTIONS = ("fixed", "disputed", "deferred")
PRIOR_FINDING_STATUSES = ("fixed", "not_fixed", "withdrawn")


class FindingResponse(Contract):
    """Implementer's answer to one finding of the previous review round."""

    id: str
    action: str
    rationale: str = ""


class PriorFindingVerdict(Contract):
    """Reviewer's ruling on one of its own blocking findings from the previous round."""

    id: str
    status: str
    evidence: str = ""


class ImplementationResult(Contract):
    status: Literal["completed", "failed", "blocked"] = "completed"
    summary: str = ""
    files_changed: list[str] = Field(default_factory=list, alias="filesChanged")
    tests: TestSummary | None = None
    assumptions: list[str] = Field(default_factory=list)
    known_issues: list[str] = Field(default_factory=list, alias="knownIssues")
    finding_responses: list[FindingResponse] = Field(default_factory=list, alias="findingResponses")
    checkpoint_sha: str | None = Field(None, alias="checkpointSha")


class ReviewResult(Contract):
    status: Literal["completed", "failed"] = "completed"
    reviewed_sha: str | None = Field(None, alias="reviewedSha")
    verdict: Literal["approved", "changes_required"] | None = None
    summary: str = ""
    findings: list[Finding] = Field(default_factory=list)
    prior_findings: list[PriorFindingVerdict] = Field(default_factory=list, alias="priorFindings")


class DiscoveryResult(Contract):
    """Architect discovery completion. The prose lives in the requirements/ files."""

    status: Literal["completed", "failed", "blocked"] = "completed"
    summary: str = ""
    complexity: str | None = None
    affected_components: list[str] = Field(default_factory=list, alias="affectedComponents")
    assumptions: list[str] = Field(default_factory=list)
    risks: list[str] = Field(default_factory=list)
    open_questions: list[str] = Field(default_factory=list, alias="openQuestions")


class FinalAssessment(Contract):
    status: Literal["completed", "failed"] = "completed"
    task_summary: str = Field("", alias="taskSummary")
    complexity: str | None = None
    complexity_score: int | None = Field(None, alias="complexityScore")
    original_scope: str | None = Field(None, alias="originalScope")
    implementation_performed: str | None = Field(None, alias="implementationPerformed")
    files_affected: list[str] = Field(default_factory=list, alias="filesAffected")
    key_findings: list[str] = Field(default_factory=list, alias="keyFindings")
    review_findings: str | None = Field(None, alias="reviewFindings")
    fixes_made: str | None = Field(None, alias="fixesMade")
    tests_performed: str | None = Field(None, alias="testsPerformed")
    known_limitations: list[str] = Field(default_factory=list, alias="knownLimitations")
    outstanding_uncertainty: list[str] = Field(default_factory=list, alias="outstandingUncertainty")
    regression_risk: str | None = Field(None, alias="regressionRisk")
    security_risk: str | None = Field(None, alias="securityRisk")
    overall_risk: Literal["LOW", "MEDIUM", "HIGH"] = Field("MEDIUM", alias="overallRisk")
    risk_factors: list[str] = Field(default_factory=list, alias="riskFactors")
    human_review_recommendation: str = Field("", alias="humanReviewRecommendation")
    requirements_verified: bool | None = Field(None, alias="requirementsVerified")

    @field_validator("overall_risk", mode="before")
    @classmethod
    def _upper(cls, value: Any) -> Any:
        return value.upper() if isinstance(value, str) else value


# --------------------------------------------------------------------------------------
# API payloads
# --------------------------------------------------------------------------------------
class AgentOverride(BaseModel):
    harness: str | None = None
    model: str | None = None
    reasoning: str | None = None


class ProjectRequest(BaseModel):
    """Creating a project, or editing one: an edit changes only the fields it sends, and null clears one."""
    name: str | None = Field(None, max_length=80)
    color: str | None = None
    description: str | None = Field(None, max_length=2000)
    default_repository_path: str | None = Field(None, alias="defaultRepositoryPath")
    default_project_relative_path: str | None = Field(None, alias="defaultProjectRelativePath")
    default_base_branch: str | None = Field(None, alias="defaultBaseBranch", max_length=255)
    agent_overrides: dict[str, AgentOverride] | None = Field(None, alias="agentOverrides")

    model_config = ConfigDict(populate_by_name=True)

    @field_validator("name", "description", "default_repository_path", "default_project_relative_path",
                     "default_base_branch")
    @classmethod
    def _blank_is_none(cls, value: str | None) -> str | None:
        return (value.strip() or None) if value is not None else None

    @field_validator("color")
    @classmethod
    def _known_color(cls, value: str | None) -> str | None:
        if value is not None and value not in PROJECT_COLORS:
            raise ValueError(f"Colour must be one of: {', '.join(PROJECT_COLORS)}")
        return value


class TaskProjectRequest(BaseModel):
    project_id: int | None = Field(alias="projectId")


def is_local_task(key: str) -> bool:
    """Local UUID identifiers cannot overlap Jira issue keys (whose suffix is numeric)."""
    if not key.upper().startswith("LOCAL-"):
        return False
    try:
        UUID(key[6:])
        return True
    except ValueError:
        return False


class CreateTaskRequest(BaseModel):
    implementation_mode: Literal["direct", "parallel"] | None = Field(None, alias="implementationMode")
    task_source: Literal["jira", "local"] = Field("jira", alias="taskSource")
    prompt: str | None = Field(None, max_length=100000)
    jira_key: str | None = Field(None, alias="jiraKey", min_length=1, max_length=64)
    jira_title: str | None = Field(None, alias="jiraTitle", max_length=500)
    repository_path: str = Field(alias="repositoryPath")
    project_relative_path: str | None = Field(None, alias="projectRelativePath")
    base_branch: str = Field(alias="baseBranch", min_length=1)
    additional_context: str | None = Field(None, alias="additionalContext")
    agent_overrides: dict[str, AgentOverride] = Field(default_factory=dict, alias="agentOverrides")
    project_id: int | None = Field(None, alias="projectId")

    model_config = ConfigDict(populate_by_name=True)

    @model_validator(mode="before")
    @classmethod
    def _ignore_key_of_local_task(cls, data: Any) -> Any:
        # A local task's key is generated, so whatever the client sent is not validated.
        if isinstance(data, dict) and data.get("taskSource", data.get("task_source")) == "local":
            return {k: v for k, v in data.items() if k not in ("jiraKey", "jira_key")}
        return data

    @field_validator("jira_key")
    @classmethod
    def _clean_key(cls, value: str | None) -> str | None:
        if value is None:
            return None
        value = value.strip().upper()
        allowed = set("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_")
        if not value or any(ch not in allowed for ch in value):
            raise ValueError("Jira key may only contain letters, digits, '-' and '_'")
        return value

    @model_validator(mode="after")
    def _validate_source(self):
        if self.task_source == "local":
            self.prompt = (self.prompt or "").strip()
            if not self.prompt:
                raise ValueError("A local task requires a prompt")
        elif not self.jira_key or is_local_task(self.jira_key):
            raise ValueError("A Jira task requires a Jira key; local identifiers are reserved")
        return self

    @field_validator("jira_title")
    @classmethod
    def _clean_title(cls, value: str | None) -> str | None:
        return value.strip() or None if value is not None else None


class MeteredApproval(BaseModel):
    harness: str
    model: str | None = None
    reason: str = Field(min_length=1, max_length=1000)


class RunNodeRequest(BaseModel):
    override: AgentOverride | None = None
    metered_approval: MeteredApproval | None = Field(None, alias="meteredApproval")


class QuestionRequest(BaseModel):
    """A flat question (text choices) or a decision tree (object choices, `context`, `risk`; see
    `decisions`). `choices` ends up as the top-level labels either way, `decision` as the tree."""

    role: str
    message: str = Field(min_length=1, max_length=4000)
    choices: list[Any] = Field(default_factory=list, max_length=8)
    context: Any = None
    risk: Any = None
    run_id: int | None = Field(None, alias="runId")
    decision: dict[str, Any] | None = Field(None, exclude=True)
    model_config = ConfigDict(populate_by_name=True)

    @field_validator("message")
    @classmethod
    def _clean_question(cls, value: str) -> str:
        value = value.strip()
        if not value:
            raise ValueError("Question message cannot be empty")
        return value

    @model_validator(mode="after")
    def _clean_choices(self) -> "QuestionRequest":
        if decisions.is_tree(self.choices, self.context, self.risk):
            self.decision = decisions.clean_decision(self.choices, self.context, self.risk)
            self.choices = [choice["label"] for choice in self.decision["choices"]]
        else:
            self.decision = None
            self.choices = self._clean_text_choices(self.choices)
        return self

    @staticmethod
    def _clean_text_choices(value: list[Any]) -> list[str]:
        if any(not isinstance(choice, str) for choice in value):
            raise ValueError("Each choice must contain 1 to 250 characters")
        choices = [choice.strip() for choice in value]
        if any(
            not choice
            or len(choice) > 250
            or any(not char.isprintable() for char in choice)
            for choice in choices
        ):
            raise ValueError("Each choice must contain 1 to 250 characters")
        if len({choice.casefold() for choice in choices}) != len(choices):
            raise ValueError("Question choices must be unique")
        return choices


MAX_ANSWER_LENGTH = 4000


def clean_answer(value: str) -> str:
    """An answer is typed into the agent's pane followed by Enter, so it must be one printable line.
    The engine applies this too, so callers that bypass the HTTP model get the same guarantee."""
    if len(value) > MAX_ANSWER_LENGTH:
        raise ValueError(f"Answer must be at most {MAX_ANSWER_LENGTH} characters")
    if any(not char.isprintable() for char in value):
        raise ValueError("Answer must be a single line")
    value = value.strip()
    if not value:
        raise ValueError("Answer cannot be empty")
    return value


MAX_FEEDBACK_LENGTH = 4000
MAX_PR_URL_LENGTH = 2000


def clean_pr_url(value: str) -> str:
    """The pull request link recorded on a task. The engine applies this too, so callers that
    bypass the HTTP model (chat) get the same check."""
    value = value.strip()
    if not value.startswith(("http://", "https://")):
        raise ValueError("PR URL must start with http:// or https://")
    if not 8 <= len(value) <= MAX_PR_URL_LENGTH or any(not char.isprintable() for char in value):
        raise ValueError(f"PR URL must be one line of 8 to {MAX_PR_URL_LENGTH} characters")
    return value


class QuestionAnswerRequest(BaseModel):
    """Free text (`answer`) or a path of 0-based choice indexes (`path`, branch then follow-up
    option); the engine builds the line typed into the agent's pane from either."""

    answer: str | None = Field(None, min_length=1, max_length=MAX_ANSWER_LENGTH)
    path: list[Annotated[StrictInt, Field(ge=0)]] | None = Field(None, min_length=1, max_length=2)

    @field_validator("answer")
    @classmethod
    def _clean_answer(cls, value: str | None) -> str | None:
        return clean_answer(value) if value is not None else None

    @model_validator(mode="after")
    def _one_of(self) -> "QuestionAnswerRequest":
        if (self.answer is None) == (self.path is None):
            raise ValueError("Send either answer or path")
        return self


class CompleteRequest(BaseModel):
    role: str
    run_id: int | None = Field(None, alias="runId")
    result: dict[str, Any]
    model_config = ConfigDict(populate_by_name=True)


class FailRequest(BaseModel):
    role: str
    run_id: int | None = Field(None, alias="runId")
    message: str
    model_config = ConfigDict(populate_by_name=True)


class ExitedRequest(BaseModel):
    role: str
    run_id: int | None = Field(None, alias="runId")
    exit_code: int = Field(alias="exitCode")
    model_config = ConfigDict(populate_by_name=True)


class RunningRequest(BaseModel):
    role: str
    run_id: int | None = Field(None, alias="runId")
    model_config = ConfigDict(populate_by_name=True)


class HeartbeatRequest(RunningRequest):
    phase: Literal["working", "investigating", "implementing", "testing", "reviewing"] = "working"


class FindingRequest(BaseModel):
    role: str
    run_id: int | None = Field(None, alias="runId")
    finding: Finding
    model_config = ConfigDict(populate_by_name=True)
