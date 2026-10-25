//! Enumerations shared by engine, API and UI, ported from `backend/agentflow/workflow/states.py`.
//! Stored and sent as the same strings Python uses.

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),* }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),* }
            }

            pub fn parse(text: &str) -> Option<$name> {
                match text { $($text => Some($name::$variant),)* _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum! {
    NodeState {
        Pending = "PENDING",
        Ready = "READY",
        Running = "RUNNING",
        WaitingForUser = "WAITING_FOR_USER",
        WaitingForApproval = "WAITING_FOR_APPROVAL",
        Completed = "COMPLETED",
        Failed = "FAILED",
        Skipped = "SKIPPED",
        Cancelled = "CANCELLED",
        Interrupted = "INTERRUPTED",
    }
}

string_enum! {
    TaskStatus {
        Active = "ACTIVE",
        NeedsHumanReview = "NEEDS_HUMAN_REVIEW",
        ReadyForPr = "READY_FOR_PR",
        PrCreated = "PR_CREATED",
        Merged = "MERGED",
        Failed = "FAILED",
        Cancelled = "CANCELLED",
    }
}

string_enum! {
    NodeKind {
        Agent = "agent",
        Approval = "approval",
        System = "system",
        Terminal = "terminal",
    }
}

string_enum! {
    Role {
        Architect = "architect",
        Orchestrator = "orchestrator",
        Implementer = "implementer",
        CodeReviewer = "code_reviewer",
        SecurityReviewer = "security_reviewer",
    }
}

impl Role {
    /// ROLE_LABELS: the tab name and the label in log messages.
    pub fn label(self) -> &'static str {
        match self {
            Role::Orchestrator => "Orchestrator",
            Role::Architect => "Architect",
            Role::Implementer => "Implementer",
            Role::CodeReviewer => "Code Review",
            Role::SecurityReviewer => "Security",
        }
    }

    /// ROLE_RUN_DIRS: the run directories under `<task>/runs/`.
    pub fn run_dir(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator",
            Role::Architect => "architect",
            Role::Implementer => "implementer",
            Role::CodeReviewer => "code-review",
            Role::SecurityReviewer => "security-review",
        }
    }
}

impl NodeState {
    /// ACTIVE_STATES: an agent process is expected to report back.
    pub fn is_active(self) -> bool {
        matches!(self, NodeState::Running | NodeState::WaitingForUser)
    }
}

impl TaskStatus {
    /// POST_READY_STATUSES: Ready for PR or further along; no more nodes, approvals or agent
    /// signals are accepted, regardless of PR/merge progress.
    pub fn is_post_ready(self) -> bool {
        matches!(self, TaskStatus::ReadyForPr | TaskStatus::PrCreated | TaskStatus::Merged)
    }

    /// FINISHED_STATUSES: nothing more will happen; the task may be archived or deleted.
    pub fn is_finished(self) -> bool {
        matches!(self, TaskStatus::Merged | TaskStatus::Cancelled | TaskStatus::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_round_trip() {
        for state in NodeState::ALL {
            assert_eq!(NodeState::parse(state.as_str()), Some(*state));
        }
        assert_eq!(TaskStatus::parse("READY_FOR_PR"), Some(TaskStatus::ReadyForPr));
        assert_eq!(Role::parse("code_reviewer").map(Role::label), Some("Code Review"));
        assert_eq!(NodeKind::parse("nope"), None);
    }
}
