//! Slack form serialization and extraction. Submission authorization belongs to the service.
use super::{
    models::{TaskDraft, TaskForm, TextForm},
    render::{self, plain},
};
use serde_json::{Value, json};

fn searchable(block: &str, label: &str, placeholder: &str, initial: Option<&str>, hint: Option<&str>) -> Value {
    let mut element = json!({"type":"external_select","action_id":format!("agentflow_task_{block}"),"min_query_length":0,"placeholder":plain(placeholder,150)});
    if let Some(option) = initial.filter(|s| !s.is_empty()).and_then(|s| render::option(s, s)) {
        element["initial_option"] = option;
    }
    let mut value = json!({"type":"input","block_id":block,"label":plain(label,2000),"element":element});
    if let Some(hint) = hint {
        value["hint"] = plain(hint, 2000);
    }
    value
}
fn input(block: &str, label: &str, initial: Option<&str>, multiline: bool, max_length: usize) -> Value {
    let mut element =
        json!({"type":"plain_text_input","action_id":"value","multiline":multiline,"max_length":max_length});
    if let Some(initial) = initial.filter(|s| !s.is_empty()) {
        element["initial_value"] = initial.into();
    }
    json!({"type":"input","block_id":block,"label":plain(label,2000),"element":element})
}
pub fn task_modal(form: &TaskForm) -> Value {
    let ticket = if form.search_tickets {
        searchable("ticket", "Jira ticket", "Search Jira or type a key", form.jira_key.as_deref(), None)
    } else {
        input("ticket", "Jira key", form.jira_key.as_deref(), false, 64)
    };
    let options = vec![
        json!({"text":plain("Direct: one implementer",75),"value":"direct"}),
        json!({"text":plain("Parallel workers",75),"value":"parallel"}),
    ];
    let initial =
        options.iter().find(|o| o["value"] == form.implementation_mode).cloned().unwrap_or_else(|| options[0].clone());
    let mut project = searchable(
        "project",
        "Project sub-directory",
        "Search the repository's folders or type one",
        None,
        Some(
            "Relative to the repository root; choose the repository first. Leave empty, or choose Whole repository, for all of it.",
        ),
    );
    project["optional"] = true.into();
    let mut context = input("context", "Additional context for the Architect", None, true, 3000);
    context["optional"] = true.into();
    json!({"type":"modal","callback_id":"agentflow_task_form","title":plain("New AgentFlow task",24),"submit":plain("Create",24),"close":plain("Cancel",24),"blocks":[
        ticket,searchable("repository","Repository","Search past repositories or paste a path",form.repository.as_deref(),Some("A folder on the AgentFlow machine.")),
        searchable("branch","Base branch","Search branches or type one",Some(&form.base_branch),Some("Choose the repository first; the list shows its local and remote branches.")),
        project,json!({"type":"input","block_id":"mode","label":plain("Implementation route",2000),"element":{"type":"static_select","action_id":"value","options":options,"initial_option":initial}}),context]})
}
pub fn selected<'a>(values: &'a Value, block: &str) -> &'a str {
    let Some(element) = values[block].as_object().and_then(|m| m.values().next()) else {
        return "";
    };
    element["selected_option"]["value"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| element["value"].as_str())
        .unwrap_or("")
}
fn optional(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.into())
}
pub fn task_draft(user: &str, values: &Value) -> TaskDraft {
    TaskDraft {
        user_id: user.into(),
        jira_key: selected(values, "ticket").into(),
        repository_path: selected(values, "repository").into(),
        base_branch: selected(values, "branch").into(),
        project_relative_path: optional(selected(values, "project")),
        implementation_mode: optional(selected(values, "mode")),
        additional_context: optional(selected(values, "context")),
    }
}
pub fn answer_modal(question_blocks: &[Value], metadata: &Value) -> Value {
    let mut blocks = question_blocks.to_vec();
    blocks.push(input("answer", "Your answer (one line)", None, false, 4000));
    json!({"type":"modal","callback_id":"agentflow_answer","title":plain("Answer the agent",24),"submit":plain("Send",24),"close":plain("Cancel",24),"private_metadata":pyjson::dumps_compact(metadata),"blocks":blocks})
}
pub fn text_modal(form: &TextForm) -> Value {
    json!({"type":"modal","callback_id":"agentflow_text_form","title":plain(&form.title,24),"submit":plain("Send",24),"close":plain("Cancel",24),
        "private_metadata":pyjson::dumps_compact(&json!({"kind":form.kind,"value":form.value})),"blocks":[{"type":"section","text":plain(&form.intro,render::SECTION_LIMIT)},input("text",&form.label,None,form.multiline,form.max_length)]})
}
