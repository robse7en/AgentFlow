use agentflow_core::chat::actions::run_buttons;
use agentflow_core::config::{SlackAction, SlackConfig};
use agentflow_core::engine::lifecycle::{CreateTaskRequest, is_local_task};
use serde_json::json;

#[test]
fn local_prompt_is_required_and_identifiers_are_generated() {
    let request = |prompt: &str| {
        serde_json::from_value::<CreateTaskRequest>(json!({
            "taskSource": "local", "prompt": prompt, "repositoryPath": ".", "baseBranch": "main",
            "jiraKey": "attacker-controlled", "additionalContext": "Extra details",
        }))
        .unwrap()
        .validated()
    };
    assert!(request(" ").is_err());
    let first = request("  Fix the editor\nKeep keyboard support ").unwrap();
    let second = request("Fix the editor").unwrap();
    assert!(is_local_task(&first.jira_key));
    assert_ne!(first.jira_key, second.jira_key);
    assert_eq!(first.jira_title.as_deref(), Some("Fix the editor"));
    assert_eq!(first.additional_context.as_deref(), Some("Fix the editor\nKeep keyboard support\n\nExtra details"));
}

#[test]
fn local_title_is_the_first_line_cut_to_120_characters_and_blank_context_is_dropped() {
    let request: CreateTaskRequest = serde_json::from_value(json!({
        "taskSource": "local", "prompt": format!("{}\nmore", "é".repeat(130)), "repositoryPath": ".",
        "baseBranch": "main", "additionalContext": "  \n ",
    }))
    .unwrap();
    let request = request.validated().unwrap();
    assert_eq!(request.jira_title, Some("é".repeat(120)));
    assert_eq!(request.additional_context, Some(format!("{}\nmore", "é".repeat(130))));
}

#[test]
fn prompt_is_limited_to_100000_characters() {
    let request = |length: usize| {
        serde_json::from_value::<CreateTaskRequest>(json!({
            "taskSource": "local", "prompt": "x".repeat(length), "repositoryPath": ".", "baseBranch": "main",
        }))
        .unwrap()
        .validated()
    };
    assert!(request(100_000).is_ok());
    assert!(request(100_001).unwrap_err().contains("prompt"));
}

#[test]
fn jira_cannot_impersonate_local_identifiers() {
    let request: CreateTaskRequest = serde_json::from_value(json!({
        "jiraKey": "LOCAL-550e8400-e29b-41d4-a716-446655440000", "repositoryPath": ".", "baseBranch": "main",
    }))
    .unwrap();
    assert!(request.validated().unwrap_err().contains("reserved"));
    // A Jira project may still be called LOCAL: only the UUID form is reserved.
    assert!(!is_local_task("LOCAL-123"));
}

#[test]
fn running_a_local_architect_from_slack_does_not_mention_a_jira_transition() {
    let settings = SlackConfig { allow_actions: vec![SlackAction::Run], ..Default::default() };
    let snapshot = |source: &str| {
        json!({
            "jiraKey": "LOCAL-550E8400-E29B-41D4-A716-446655440000", "taskSource": source, "status": "ACTIVE",
            "nodes": [{"key": "architect", "label": "Architect", "state": "READY",
                       "effectiveConfig": {"harnessAvailable": true, "billingMode": "subscription"}}],
        })
    };
    let confirm =
        |source: &str| run_buttons(&snapshot(source), &settings, Some("In Progress"))[0].confirm.clone().unwrap();
    assert!(!confirm("local").contains("Jira"), "{}", confirm("local"));
    assert!(confirm("jira").contains("moves the Jira ticket to In Progress"));
}
