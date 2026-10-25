//! Chat answers use the engine's normal validation, run checks and audit trail.
use super::models::{AnswerPrompt, AnswerRequest};
use crate::{
    config::SlackConfig,
    engine::{Engine, decisions},
};

const POSTABLE: &[&str] = &[
    "This question has already been answered",
    "The agent is no longer waiting for this answer",
    "Question is not attached to an active agent run",
    "The waiting agent pane is unavailable",
    "The waiting agent pane is no longer active",
    "The Zellij session for this agent is unavailable",
    "Answer must be a single line",
    "Answer cannot be empty",
    "Answer must be at most 4000 characters",
];

/// A fixed, safe refusal for the operator, or None once the answer was delivered (or a decision
/// tree's control showed a prompt instead; see `respond`).
pub async fn answer(
    engine: &Engine,
    settings: &SlackConfig,
    channel: Option<&str>,
    request: &AnswerRequest,
) -> Option<String> {
    respond(engine, settings, channel, request).await.err()
}

/// Ok(None) once the answer was delivered, Ok(prompt) when a decision tree's branch has a
/// follow-up (or for "Change branch") and the notice should show that prompt instead, else a
/// fixed, safe refusal for the operator.
pub async fn respond(
    engine: &Engine,
    settings: &SlackConfig,
    channel: Option<&str>,
    request: &AnswerRequest,
) -> Result<Option<AnswerPrompt>, String> {
    let refused = |reason: &str| Err(reason.to_string());
    if settings.user_id.as_deref() != Some(request.user_id.as_str()) {
        return refused("You are not authorized to answer AgentFlow questions.");
    }
    if !settings.answering() {
        return refused("Answering from Slack is turned off; answer in AgentFlow.");
    }
    if channel != Some(request.channel.as_str()) {
        return refused("Answer from your AgentFlow direct messages.");
    }
    let Ok(snapshot) = engine.snapshot(&request.jira_key) else {
        return refused("That task no longer exists.");
    };
    let Some(question) = snapshot["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|q| q["uid"].as_str() == Some(&request.question_uid))
    else {
        return refused("That question is no longer open.");
    };
    let no_choice = "That choice is no longer offered.";
    let decision = Some(&question["decision"]).filter(|d| !d.is_null());
    let mut path: Option<Vec<usize>> = None;
    let mut text = "";
    match decision {
        Some(decision) if request.choice_index.is_some() || request.path.is_some() || request.back => {
            if question["actionable"] != true {
                return refused("That question is no longer open.");
            }
            let choices = question["choices"].as_array().into_iter().flatten();
            let prompt = AnswerPrompt {
                jira_key: request.jira_key.clone(),
                question_uid: request.question_uid.clone(),
                choices: choices.filter_map(|c| c.as_str().map(String::from)).collect(),
                decision: Some(decision.clone()),
                branch: None,
            };
            if request.back {
                return Ok(Some(prompt));
            }
            let branches = decision["choices"].as_array().map(Vec::as_slice).unwrap_or_default();
            let branch = request.path.map(|(branch, _)| branch).or(request.choice_index).unwrap_or(usize::MAX);
            let Some(chosen) = branches.get(branch) else { return refused(no_choice) };
            if request.path.is_none() && !chosen["followUp"].is_null() {
                return Ok(Some(AnswerPrompt { branch: Some(branch), ..prompt }));
            }
            path = Some(request.path.map_or(vec![branch], |(branch, option)| vec![branch, option]));
        }
        _ if request.choice_index.is_some() => {
            let index = request.choice_index.unwrap_or_default();
            match question["choices"].as_array().and_then(|a| a.get(index)).and_then(|s| s.as_str()) {
                Some(choice) => text = choice,
                None => return refused(no_choice),
            }
        }
        _ if request.path.is_some() || request.back => return refused(no_choice),
        _ => text = request.text.as_deref().unwrap_or(""),
    }
    let sent = match &path {
        Some(path) => engine.answer_question_path(&request.jira_key, &request.question_uid, path, Some("Slack")).await,
        None => engine.answer_question(&request.jira_key, &request.question_uid, text, Some("Slack")).await,
    };
    match sent {
        Ok(_) => Ok(None),
        Err(error) if error.status == 404 => refused("That question is no longer open."),
        Err(error) if [decisions::NO_SUCH_CHOICE, decisions::NEEDS_FOLLOW_UP].contains(&error.message.as_str()) => {
            refused(no_choice)
        }
        Err(error) if POSTABLE.contains(&error.message.as_str()) => Err(format!("Not sent: {}.", error.message)),
        Err(_) => refused("Not sent: AgentFlow could not deliver the answer. Check AgentFlow."),
    }
}
