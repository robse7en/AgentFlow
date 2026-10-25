//! One typed row per table of `backend/agentflow/database/models.py`, read by column name.
//! JSON columns are [`Json`]; a nullable one reads `Value::Null` for SQL NULL or JSON null.

use super::{DbTime, Json};

/// Declares a row struct whose field names are its column names (`field @ "column"` where the
/// column name is a Rust keyword), with the table name, the column list, `from_row` and a
/// [`super::table::Row`] implementation. The first field is the primary key: an `i64` key is the
/// database-assigned `INTEGER PRIMARY KEY`, a `String` key is a uuid the code supplies.
macro_rules! row {
    ($(#[$meta:meta])* $name:ident = $table:literal {
        $(#[$key_meta:meta])* $key:ident : $key_ty:ty,
        $($(#[$field_meta:meta])* $field:ident $(@ $column:literal)? : $ty:ty),* $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $(#[$key_meta])* pub $key: $key_ty,
            $($(#[$field_meta])* pub $field: $ty),*
        }

        impl $name {
            pub const TABLE: &'static str = $table;
            pub const COLUMNS: &'static [&'static str] =
                &[stringify!($key), $(row!(@column $field $(, $column)?)),*];

            pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
                Ok($name { $key: row.get(stringify!($key))?, $($field: row.get(row!(@column $field $(, $column)?))?),* })
            }
        }

        impl super::table::Row for $name {
            const TABLE: &'static str = $table;
            const COLUMNS: &'static [&'static str] = $name::COLUMNS;
            const AUTO_KEY: bool = <$key_ty as super::table::KeyKind>::AUTO;

            fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
                $name::from_row(row)
            }

            fn values(&self) -> rusqlite::Result<Vec<rusqlite::types::Value>> {
                Ok(vec![super::table::owned(&self.$key)?, $(super::table::owned(&self.$field)?),*])
            }

            fn set_key(&mut self, id: i64) {
                <$key_ty as super::table::KeyKind>::assign(&mut self.$key, id);
            }
        }
    };
    (@column $field:ident, $column:literal) => { $column };
    (@column $field:ident) => { stringify!($field) };
}

row! {
    RequirementsApprovalRow = "requirements_approvals" {
        uid: String,
        task_id: i64,
        digest: String,
        documents: Json,
        approved_at: DbTime,
    }
}

row! {
    RunEvidenceRow = "run_evidence" {
        uid: String,
        run_id: i64,
        requirements_digest: Option<String>,
        verification: Json,
        usage: Json,
        confirmed_model: Option<String>,
        billing_mode: String,
    }
}

row! {
    /// What a deleted task's runs cost, so deleting a task never frees daily API budget.
    DeletedRunSpendRow = "deleted_run_spend" {
        id: i64,
        jira_key: String,
        run_id: i64,
        charged_at: DbTime,
        billing_mode: String,
        cost_usd: Option<f64>,
    }
}

row! {
    TaskRow = "tasks" {
        id: i64,
        jira_key: String,
        api_budget_usd: Option<f64>,
        jira_title: Option<String>,
        repository_path: String,
        repository_root: String,
        project_relative_path: Option<String>,
        base_branch: String,
        branch_name: String,
        worktree_path: String,
        zellij_session_name: String,
        task_dir: String,
        additional_context: Option<String>,
        status: String,
        status_message: Option<String>,
        review_iteration: i64,
        max_review_iterations: i64,
        max_requirements_revisions: i64,
        current_sha: Option<String>,
        pr_url: Option<String>,
        pr_created_at: Option<DbTime>,
        merged_at: Option<DbTime>,
        /// Set when the human undoes a PR status; PR sync then leaves the task alone.
        pr_sync_paused: bool,
        /// Head commit of the PR as GitHub merged it.
        merged_head_sha: Option<String>,
        /// The branch the recorded PR targets, as GitHub last reported it (None: not looked up).
        pr_base_branch: Option<String>,
        /// Hidden from the dashboard and chat task lists; set only on a finished task.
        archived_at: Option<DbTime>,
        /// Deleting a project clears this first (`Engine::delete_project`); nothing cascades.
        project_id: Option<i64>,
        /// role -> {"harness", "model", "reasoning"} (task-level overrides)
        agent_overrides: Json,
        created_at: DbTime,
        updated_at: DbTime,
    }
}

row! {
    /// A named group of tasks, with defaults the New Task form copies when the project is picked.
    ProjectRow = "projects" {
        id: i64,
        /// The URL token; set from the name at creation and kept through renames.
        slug: String,
        name: String,
        color: String,
        description: Option<String>,
        default_repository_path: Option<String>,
        default_project_relative_path: Option<String>,
        default_base_branch: Option<String>,
        /// role -> {"harness", "model", "reasoning"}, as on the task
        agent_overrides: Json,
        /// Hidden from the pickers; its tasks keep it.
        archived_at: Option<DbTime>,
        created_at: DbTime,
        updated_at: DbTime,
    }
}

row! {
    WorkflowNodeRow = "workflow_nodes" {
        id: i64,
        task_id: i64,
        key: String,
        /// agent | approval | system | terminal
        kind: String,
        label: String,
        role: Option<String>,
        state: String,
        execution_mode: String,
        iteration: i64,
        checkpoint_sha: Option<String>,
        message: Option<String>,
        error: Option<String>,
        config_override: Json,
        updated_at: DbTime,
    }
}

row! {
    NodeRunRow = "node_runs" {
        id: i64,
        task_id: i64,
        node_id: i64,
        run_number: i64,
        iteration: i64,
        state: String,
        harness: Option<String>,
        model: Option<String>,
        reasoning: Option<String>,
        run_dir: Option<String>,
        prompt_path: Option<String>,
        result_path: Option<String>,
        expected_sha: Option<String>,
        result: Json,
        exit_code: Option<i64>,
        error: Option<String>,
        started_at: DbTime,
        completed_at: Option<DbTime>,
    }
}

row! {
    AgentSessionRow = "agent_sessions" {
        id: i64,
        task_id: i64,
        role: String,
        harness: String,
        model: Option<String>,
        reasoning: Option<String>,
        zellij_tab_name: String,
        zellij_tab_id: Option<i64>,
        zellij_pane_id: Option<String>,
        harness_session_id: Option<String>,
        /// running | exited | stopped
        status: String,
        /// idle | working | blocked | unknown, once the screen has been classified.
        screen_state: Option<String>,
        screen_state_at: Option<DbTime>,
        current_run_id: Option<i64>,
        started_at: DbTime,
        ended_at: Option<DbTime>,
    }
}

row! {
    ReviewFindingRow = "review_findings" {
        id: i64,
        task_id: i64,
        node_run_id: Option<i64>,
        /// CODE-001 / SEC-001
        finding_id: String,
        /// code-review | security-review
        kind @ "type": String,
        iteration: i64,
        reviewed_sha: Option<String>,
        severity: String,
        blocking: bool,
        file: Option<String>,
        line: Option<i64>,
        title: String,
        description: Option<String>,
        recommendation: Option<String>,
        /// open | resolved | withdrawn | superseded | accepted
        status: String,
        /// Lineage: a re-raised finding keeps its id and points at the row it continues.
        prior_finding_id: Option<i64>,
        occurrence: i64,
        /// The reviewer's evidence for its verdict.
        resolution: Option<String>,
        /// The implementer's response: fixed | disputed | deferred
        response: Option<String>,
        response_note: Option<String>,
        created_at: DbTime,
        resolved_at: Option<DbTime>,
    }
}

row! {
    WorkflowEventRow = "workflow_events" {
        id: i64,
        task_id: i64,
        timestamp: DbTime,
        level: String,
        node_key: Option<String>,
        message: String,
        data: Json,
    }
}

row! {
    TaskArtifactRow = "task_artifacts" {
        id: i64,
        task_id: i64,
        node_run_id: Option<i64>,
        kind: String,
        path: String,
        created_at: DbTime,
    }
}

row! {
    AgentQuestionRow = "agent_questions" {
        id: i64,
        uid: String,
        task_id: i64,
        node_run_id: Option<i64>,
        role: String,
        message: String,
        choices: Json,
        answered: bool,
        answer: Option<String>,
        created_at: DbTime,
        answered_at: Option<DbTime>,
        /// A decision tree's `{context, risk, choices}`; `Value::Null` for a flat question.
        decision: Json,
    }
}

row! {
    /// A run's scoped agent credential (only its hash is stored).
    AgentAccessRow = "agent_access" {
        id: String,
        task_id: i64,
        run_id: i64,
        role: String,
        token_hash: String,
    }
}

row! {
    /// Binds exit reports to the launched session, even when it serves later runs.
    ProcessLeaseRow = "process_leases" {
        id: String,
        session_id: i64,
        launch_run_id: i64,
    }
}

row! {
    /// Durable worker identity and briefing; lifecycle remains on its workflow node.
    ImplementationWorkItemRow = "implementation_work_items" {
        uid: String,
        task_id: i64,
        node_id: i64,
        plan_key: String,
        brief: String,
        owned_paths: Json,
        acceptance_criteria: Json,
        dependencies: Json,
        worktree_path: String,
        branch_name: String,
        base_sha: String,
        prepared: bool,
    }
}

row! {
    /// Where a task's chat notifications go, and which conditions were already announced.
    ChatThreadRow = "chat_threads" {
        id: i64,
        task_id: i64,
        platform: String,
        channel: Option<String>,
        thread_id: Option<String>,
        title: Option<String>,
        notified: Json,
        updated_at: DbTime,
    }
}
