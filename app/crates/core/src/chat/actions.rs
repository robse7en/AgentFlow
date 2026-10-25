//! Typed commands offer buttons; only authorized DM button callbacks mutate the engine.
use super::{details, models::*, render, rich, service::ChatService};
use crate::{
    config::{SlackAction, SlackConfig},
    engine::{WorkflowError, approvals::ApprovalRequest, launch::RunNodeRequest, rules, spec},
};
use serde_json::Value;
pub(crate) fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
pub fn button(kind: &str, label: &str, value: &str, confirm: Option<String>) -> Button {
    Button { kind: kind.into(), label: label.into(), value: value.into(), confirm }
}
pub fn gate_waiting(snapshot: &Value) -> bool {
    snapshot["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|n| n["key"] == spec::REQUIREMENTS_GATE && n["state"] == "WAITING_FOR_APPROVAL")
}
pub fn launch_refusal(snapshot: &Value, node: &Value) -> Option<String> {
    let label = if s(node, "label").is_empty() { s(node, "key") } else { s(node, "label") };
    if snapshot["status"] != "ACTIVE" {
        return Some(format!("{} is {}; start stages in AgentFlow.", s(snapshot, "jiraKey"), s(snapshot, "status")));
    }
    if node["state"] != "READY" || !node["effectiveConfig"].is_object() {
        return Some(format!("{label} is {}; only a READY stage can be started from Slack.", s(node, "state")));
    }
    let config = &node["effectiveConfig"];
    if config["harnessAvailable"] == false {
        return Some(format!("{label}'s harness is unavailable; choose another in AgentFlow."));
    }
    if config["billingMode"] != "subscription" {
        if config["requiresMeteredApproval"] == true {
            return Some(format!("{label} needs API launch approval; start it in AgentFlow."));
        }
        if let Some(blocker) = rules::launch_blocker(&snapshot["budget"]) {
            return Some(format!("Not started: {blocker}."));
        }
    }
    None
}
pub fn run_buttons(snapshot: &Value, settings: &SlackConfig, transition: Option<&str>) -> Vec<Button> {
    if !settings.allows(SlackAction::Run) {
        return vec![];
    }
    snapshot["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["state"] == "READY" && launch_refusal(snapshot, n).is_none())
        .map(|n| {
            let label = if s(n, "label").is_empty() { s(n, "key") } else { s(n, "label") };
            let key = s(snapshot, "jiraKey");
            let mut confirm = format!("Start {label} for {key}?");
            if n["key"] == spec::ARCHITECT
                && snapshot["taskSource"] != "local"
                && let Some(transition) = transition
            {
                confirm.push_str(&format!(" This also moves the Jira ticket to {transition}."));
            }
            button("run", &format!("Run {label}"), &format!("{key}|{}", s(n, "key")), Some(confirm))
        })
        .collect()
}
pub fn gate_buttons(key: &str, settings: &SlackConfig) -> Vec<Button> {
    let mut buttons = vec![];
    if settings.allows(SlackAction::ApproveRequirements) {
        buttons.push(button("review_requirements", "Review requirements", key, None));
    }
    if settings.allows(SlackAction::RequestChanges) {
        buttons.push(button("request_changes", "Request changes…", key, None));
    }
    buttons
}
pub fn notice_buttons(
    snapshot: &Value,
    condition: &str,
    settings: &SlackConfig,
    transition: Option<&str>,
) -> Vec<Button> {
    let key = s(snapshot, "jiraKey");
    if let Some(rest) = condition.strip_prefix("node:")
        && let Some((node, state)) = rest.rsplit_once(':')
    {
        if state == "READY" {
            return run_buttons(snapshot, settings, transition)
                .into_iter()
                .filter(|b| b.value == format!("{key}|{node}"))
                .collect();
        }
        if node == spec::REQUIREMENTS_GATE && state == "WAITING_FOR_APPROVAL" {
            return gate_buttons(key, settings);
        }
    }
    if condition == "status:READY_FOR_PR" && settings.allows(SlackAction::MarkPr) {
        return vec![button("mark_pr", "Mark PR created…", key, None)];
    }
    vec![]
}
pub fn refusal(error: WorkflowError, prefix: &str) -> String {
    let starts = [
        "Task already reached",
        "Task must have a PR created",
        "Task must be Ready for PR",
        "PR URL must",
        "Feedback",
        "Requirements gate is not waiting",
        "Maximum requirement revisions",
        "Task no longer accepts",
        "Task is not marked",
        "Requirements changed since",
        "Refresh and review",
        "Requirements already approved",
        "A nonempty",
        "Stop active agents",
    ];
    if starts.iter().any(|p| error.message.starts_with(p))
        || ["is no longer ready to start", "is not waiting for approval"].iter().any(|p| error.message.ends_with(p))
    {
        format!("{prefix}: {}.", error.message)
    } else {
        format!("{prefix}: AgentFlow could not do it. Check AgentFlow.")
    }
}
impl ChatService {
    pub fn off(&self, what: &str, setting: &str) -> String {
        format!("{what} from Slack is turned off. Add \"{setting}\" to slack.allowActions in config.json.")
    }
    pub fn transition(&self) -> Option<String> {
        let jira = &self.engine.config().jira;
        if jira.missing().is_empty() { jira.auto_transition_status.clone() } else { None }
    }
    pub async fn action(&self, request: &ActionRequest) -> Option<String> {
        if !self.authorized(&request.user_id) {
            return Some("You are not authorized to use AgentFlow.".into());
        }
        if self.dm().as_deref() != Some(request.channel.as_str()) {
            return Some("Use the button in your AgentFlow direct messages.".into());
        }
        match request.kind.as_str() {
            "run" => self.start_stage(&request.value).await,
            "review_requirements" => self.review_requirements(&request.value).await,
            "approve_requirements" => self.approve_requirements(&request.value).await,
            "cancel" | "mark_merged" => {
                let action = if request.kind == "cancel" { SlackAction::Cancel } else { SlackAction::MarkPr };
                if !self.settings.allows(action) {
                    return Some(self.off(
                        if action == SlackAction::Cancel { "Cancelling tasks" } else { "Marking pull requests" },
                        if action == SlackAction::Cancel { "cancel" } else { "markPr" },
                    ));
                }
                if self.snapshot(&request.value).is_none() {
                    return Some("That task no longer exists.".into());
                }
                let result = if action == SlackAction::Cancel {
                    self.engine.cancel_task(&request.value).await
                } else {
                    self.engine.mark_pr_merged(&request.value).await
                };
                result.err().map(|e| refusal(e, "Not done"))
            }
            _ => Some("That action is not available.".into()),
        }
    }
    async fn start_stage(&self, value: &str) -> Option<String> {
        if !self.settings.allows(SlackAction::Run) {
            return Some("Starting stages from Slack is turned off; start it in AgentFlow.".into());
        }
        let (key, node) = value.split_once('|').unwrap_or((value, ""));
        let Some(snapshot) = self.snapshot(key) else {
            return Some("That task no longer exists.".into());
        };
        let Some(stage) = snapshot["nodes"].as_array().into_iter().flatten().find(|n| n["key"] == node) else {
            return Some("That stage no longer exists.".into());
        };
        if let Some(reason) = launch_refusal(&snapshot, stage) {
            return Some(reason);
        }
        if let Err(error) = self.engine.run_node(key, node, RunNodeRequest::default(), true).await {
            return Some(refusal(error, "Not started"));
        }
        let Some(after) = self.snapshot(key) else {
            return Some("Could not confirm the stage started. Check AgentFlow.".into());
        };
        let Some(state) = after["nodes"].as_array().into_iter().flatten().find(|n| n["key"] == node) else {
            return Some("Could not confirm the stage started. Check AgentFlow.".into());
        };
        if matches!(s(state, "state"), "RUNNING" | "WAITING_FOR_USER" | "WAITING_FOR_APPROVAL" | "COMPLETED") {
            None
        } else {
            let message = s(state, "message");
            Some(
                if [
                    "Global agent capacity reached; start again when a slot is free",
                    "API launch needs approval: review the selected harness and model, and enter a reason",
                ]
                .contains(&message)
                {
                    format!("Not started: {message}.")
                } else {
                    "Not started. Check AgentFlow.".into()
                },
            )
        }
    }
    async fn review_requirements(&self, key: &str) -> Option<String> {
        if !self.settings.allows(SlackAction::ApproveRequirements) {
            return Some(self.off("Approving requirements", "approveRequirements"));
        }
        let Some(snapshot) = self.snapshot(key) else {
            return Some("That task no longer exists.".into());
        };
        if !gate_waiting(&snapshot) {
            return Some(format!("{key}'s requirements are not waiting for approval."));
        }
        let documents: Vec<_> = snapshot["requirements"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, text)| {
                text.as_str().filter(|s| !s.trim().is_empty()).map(|text| (details::title(name).into(), text.into()))
            })
            .collect();
        if documents.is_empty() {
            return Some(format!("{key} has no requirements to review yet."));
        }
        if documents.iter().map(|(_, text): &(String, String)| text.chars().count()).sum::<usize>() > 60000 {
            return Some(format!(
                "{key}'s requirements are too long to review in Slack; review and approve them in AgentFlow."
            ));
        }
        let digest = s(&snapshot, "requirementsDraftDigest");
        for text in rich::requirements(key, &documents) {
            if self.post(Some(key), Reply::text(text)).await.is_err() {
                return Some("Could not post the requirements in full, so there is nothing to approve. Try again, or use AgentFlow.".into());
            }
        }
        let mut buttons = vec![button(
            "approve_requirements",
            "Approve requirements",
            &format!("{key}|{digest}"),
            Some(format!("Approve the requirements posted above for {key}? The next stage can then start.")),
        )];
        if self.settings.allows(SlackAction::RequestChanges) {
            buttons.push(button("request_changes", "Request changes…", key, None));
        }
        self.post(
            Some(key),
            Reply { text: render::escape(&format!("Approve these requirements for {key}?")), buttons, offer: None },
        )
        .await
        .err()
        .map(|_| {
            "Could not post the requirements in full, so there is nothing to approve. Try again, or use AgentFlow."
                .into()
        })
    }
    async fn approve_requirements(&self, value: &str) -> Option<String> {
        if !self.settings.allows(SlackAction::ApproveRequirements) {
            return Some(self.off("Approving requirements", "approveRequirements"));
        }
        let (key, digest) = value.split_once('|').unwrap_or((value, ""));
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Some("That approval button is not valid; use Review requirements again.".into());
        }
        let result = self
            .engine
            .approve_via(
                key,
                spec::REQUIREMENTS_GATE,
                ApprovalRequest { expected_requirements_digest: Some(digest.into()), ..Default::default() },
                Some("Slack"),
            )
            .await;
        match result {
            Err(error) => Some(refusal(error, "Not approved")),
            Ok(snapshot) => {
                let buttons = run_buttons(&snapshot, &self.settings, self.transition().as_deref());
                if !buttons.is_empty() {
                    let _ = self
                        .post(
                            Some(key),
                            Reply { text: "Requirements approved. Start the next stage:".into(), buttons, offer: None },
                        )
                        .await;
                }
                None
            }
        }
    }
    pub fn action_reply(&self, command: &str, args: &[&str], thread_key: Option<&str>) -> Reply {
        let (action, what, setting) = match command {
            "run" => (SlackAction::Run, "Starting stages", "run"),
            "changes" => (SlackAction::RequestChanges, "Requesting changes", "requestChanges"),
            "approve" => (SlackAction::ApproveRequirements, "Approving requirements", "approveRequirements"),
            "cancel" => (SlackAction::Cancel, "Cancelling tasks", "cancel"),
            _ => (SlackAction::MarkPr, "Marking pull requests", "markPr"),
        };
        if !self.settings.allows(action) {
            return Reply::text(self.off(what, setting));
        }
        let (target, rest) = if !args.is_empty()
            && (command != "run"
                || thread_key.is_none()
                || super::creation::jira_key(args[0])
                || self.snapshot(args[0]).is_some())
        {
            (Some(args[0]), &args[1..])
        } else {
            (thread_key, args)
        };
        let Some(target) = target else {
            return Reply::text(format!("Which task? Use `{command} KEY`, or ask in the task's thread."));
        };
        let Some(snapshot) = self.snapshot(target) else {
            return Reply::text(super::commands::no_task(target));
        };
        let key = s(&snapshot, "jiraKey");
        let status = s(&snapshot, "status");
        let (text, buttons) = match command {
            "run" => {
                let wanted = rest.join(" ").to_lowercase().replace(['_', '-'], " ");
                let selected: Vec<_> = snapshot["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|n| n["effectiveConfig"].is_object())
                    .filter(|n| {
                        rest.is_empty() || {
                            wanted == s(n, "key").to_lowercase().replace('_', " ")
                                || wanted == s(n, "label").to_lowercase()
                        }
                    })
                    .collect();
                if selected.is_empty() && !rest.is_empty() {
                    return Reply::text(render::escape(&format!("{key} has no stage {}.", rest[0])));
                }
                let buttons: Vec<_> = run_buttons(&snapshot, &self.settings, self.transition().as_deref())
                    .into_iter()
                    .filter(|b| selected.iter().any(|n| b.value.ends_with(&format!("|{}", s(n, "key")))))
                    .collect();
                if !buttons.is_empty() {
                    (format!("Start a stage of {key}:"), buttons)
                } else {
                    let reasons: Vec<_> = selected
                        .iter()
                        .filter(|n| !rest.is_empty() || n["state"] == "READY")
                        .filter_map(|n| launch_refusal(&snapshot, n))
                        .collect();
                    (
                        if reasons.is_empty() {
                            format!("Nothing in {key} is ready to start.")
                        } else {
                            reasons.join("\n")
                        },
                        vec![],
                    )
                }
            }
            "changes" | "approve" => {
                if !gate_waiting(&snapshot) {
                    return Reply::text(render::escape(&format!("{key}'s requirements are not waiting for approval.")));
                }
                if command == "approve" {
                    (
                        format!("Review {key}'s requirements in full, then approve them:"),
                        vec![button("review_requirements", "Review requirements", key, None)],
                    )
                } else {
                    (
                        format!("Send {key}'s requirements back to the Architect with your feedback:"),
                        vec![button("request_changes", "Request changes…", key, None)],
                    )
                }
            }
            "cancel" => {
                if matches!(status, "MERGED" | "CANCELLED") {
                    (format!("{key} is {status}; it can no longer be cancelled."), vec![])
                } else {
                    (
                        format!("Cancel {key}?"),
                        vec![button(
                            "cancel",
                            &format!("Cancel {key}"),
                            key,
                            Some(format!(
                                "Cancel {key}? Its agents are stopped. The worktree, branch and files are kept."
                            )),
                        )],
                    )
                }
            }
            _ => match status {
                "READY_FOR_PR" => {
                    (format!("Record {key}'s pull request:"), vec![button("mark_pr", "Mark PR created…", key, None)])
                }
                "PR_CREATED" => (
                    format!("Mark {key}'s pull request as merged?"),
                    vec![button(
                        "mark_merged",
                        "Mark merged",
                        key,
                        Some(format!("Mark {key}'s pull request as merged?")),
                    )],
                ),
                _ => (format!("{key} is {status}; pull requests are tracked once it is Ready for PR."), vec![]),
            },
        };
        Reply { text: render::escape(&text), buttons, offer: None }
    }
}
