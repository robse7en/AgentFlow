//! Slack rendering is pure. Escape untrusted text before adding Slack's control syntax.
use super::{
    models::{AnswerOffer, AnswerPrompt, Button, Reply},
    notices::Notice,
};
use serde_json::{Value, json};
use std::sync::LazyLock;

pub const SECTION_LIMIT: usize = 2900;
pub const BUTTON_VALUE_LIMIT: usize = 2000;
pub const REQUIREMENTS_MESSAGE_LIMIT: usize = 20000;

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
pub fn unslack(text: &str) -> String {
    static MARKUP: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"<([^<>|]*)(?:\|([^<>]*))?>").unwrap());
    MARKUP
        .replace_all(text, |m: &regex::Captures<'_>| {
            m.get(2)
                .filter(|s| !s.as_str().is_empty())
                .map(|s| s.as_str())
                .unwrap_or_else(|| m[1].trim_start_matches('!'))
                .to_owned()
        })
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("")
}
pub(crate) fn heading(snapshot: &Value) -> String {
    let mut result = format!("*{}*", escape(text(snapshot, "jiraKey")));
    let title = text(snapshot, "jiraTitle");
    if !title.is_empty() {
        result.push_str(&format!(" · {}", escape(title)));
    }
    result
}
pub(crate) fn phase(snapshot: &Value) -> String {
    let phase = text(snapshot, "phase");
    escape(if phase.is_empty() { text(snapshot, "status") } else { phase })
}
pub fn render_title(snapshot: &Value, url: &str) -> String {
    format!("{}\n{} · <{url}|Open in AgentFlow>", heading(snapshot), phase(snapshot))
}
fn quote(text: &str) -> Vec<String> {
    let escaped = escape(text);
    let mut lines: Vec<_> =
        crate::engine::prompts::py_splitlines(&escaped).into_iter().map(|s| format!("> {s}")).collect();
    if lines.is_empty() {
        lines.push("> ".into());
    }
    lines
}
pub fn render_notice(notice: &Notice, answerable: bool) -> String {
    let mut lines = vec![escape(&notice.summary)];
    if let Some(question) = notice.question.as_deref().filter(|s| !s.is_empty()) {
        lines.extend(quote(question));
    }
    if !notice.choices.is_empty() && !answerable {
        lines.push(format!(
            "Choices: {}",
            notice.choices.iter().map(|s| format!("`{}`", escape(s).replace('`', "'"))).collect::<Vec<_>>().join(" / ")
        ));
    }
    if notice.key.starts_with("question:") {
        lines.push(if answerable { "Answer below, or in AgentFlow." } else { "Answer it in AgentFlow." }.into());
    }
    lines.join("\n")
}
pub fn render_resolved(question: Option<&Value>, include_text: bool) -> String {
    let mut lines = vec![match question {
        Some(q) if q["answered"] == true => {
            if include_text {
                format!("*Answered:* {}", escape(text(q, "answer")))
            } else {
                "*Answered.*".into()
            }
        }
        _ => "*No longer waiting for an answer.*".into(),
    }];
    if include_text && let Some(q) = question {
        lines.extend(quote(text(q, "message")));
    }
    lines.join("\n")
}
/// Limits count Unicode characters, matching Python and Slack, rather than UTF-8 bytes.
pub fn chunks(text: &str, limit: usize) -> Vec<String> {
    assert!(limit >= 6, "chunk limit must accommodate a Slack escape sequence");
    let chars: Vec<_> = text.chars().collect();
    let mut remaining = chars.as_slice();
    let mut pieces = vec![];
    while remaining.len() > limit {
        let mut cut = (limit / 2..limit).rev().find(|&i| remaining[i] == '\n').unwrap_or(limit);
        if cut == limit
            && let Some(amp) = (cut.saturating_sub(5)..cut).rev().find(|&i| remaining[i] == '&')
            && !remaining[amp..cut].contains(&';')
        {
            cut = amp;
        }
        pieces.push(remaining[..cut].iter().collect());
        remaining = &remaining[cut..];
        while remaining.first() == Some(&'\n') {
            remaining = &remaining[1..];
        }
    }
    if !remaining.is_empty() || pieces.is_empty() {
        pieces.push(remaining.iter().collect());
    }
    pieces
}
pub fn sections(text: &str) -> Vec<Value> {
    chunks(text, SECTION_LIMIT)
        .into_iter()
        .map(|piece| json!({"type":"section","text":{"type":"mrkdwn","text":piece}}))
        .collect()
}
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.into()
    } else {
        format!("{}…", text.chars().take(limit - 1).collect::<String>())
    }
}
pub fn plain(text: &str, limit: usize) -> Value {
    json!({"type":"plain_text","text":truncate(text,limit)})
}
pub fn notice_blocks(text: &str, prompt: &AnswerPrompt) -> Vec<Value> {
    let mut blocks = sections(text);
    blocks.extend(answer_blocks(prompt));
    blocks
}
/// The "You picked …" section shown with a branch's follow-up options.
pub const FOLLOW_UP_BLOCK: &str = "followup";
const CONTEXT_ELEMENTS: usize = 10;
fn risk_label(risk: &str) -> Option<&'static str> {
    match risk {
        "low" => Some("Low risk"),
        "medium" => Some("Medium risk"),
        "high" => Some("High risk"),
        _ => None,
    }
}
/// Rendered mrkdwn shortened to `limit`, never cutting an escape sequence such as `&amp;`.
fn cut(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.into();
    }
    let mut end = limit - 1;
    if let Some(amp) = (end.saturating_sub(5)..end).rev().find(|&i| chars[i] == '&')
        && !chars[amp..end].contains(&';')
    {
        end = amp;
    }
    format!("{}…", chars[..end].iter().collect::<String>())
}
/// A choice's consequences and agent's note as one context line, e.g. `3: … · ⚠ …`.
fn choice_notes(number: &str, choice: &Value) -> Option<String> {
    let mut parts = vec![];
    if !text(choice, "detail").is_empty() {
        parts.push(escape(text(choice, "detail")));
    }
    if !text(choice, "warning").is_empty() {
        parts.push(format!("⚠ {}", escape(text(choice, "warning"))));
    }
    if !text(choice, "note").is_empty() {
        let source = text(choice, "source");
        let source = if source.is_empty() { String::new() } else { format!(" ({})", escape(source)) };
        parts.push(format!(
            "⚠ Note on \"{}\": {}{source}",
            escape(text(choice, "label")),
            escape(text(choice, "note"))
        ));
    }
    (!parts.is_empty()).then(|| format!("{number}: {}", parts.join(" · ")))
}
/// The answer controls under a question notice (`answer_blocks` in slack.py). A flat question gets
/// one button per choice; a decision tree its numbered branches (the agent's pick in green, `›`
/// where a follow-up comes next) with their consequences in a context block, or, with
/// `prompt.branch`, that branch's follow-up options and "‹ Change branch". Buttons carry the path
/// (`1`, `0.1`, `back`), never text.
pub fn answer_blocks(prompt: &AnswerPrompt) -> Vec<Value> {
    let reference = format!("{}|{}", prompt.jira_key, prompt.question_uid);
    let button = |action_id: String, label: &str, value: &str, primary: bool| {
        let mut element = json!({"type":"button","action_id":action_id,"text":plain(label,75),"value":format!("{reference}|{value}")});
        if primary {
            element["style"] = "primary".into();
        }
        element
    };
    let recommended = |choice: &Value| choice["recommended"] == true;
    let has_follow_up = |choice: &Value| choice.get("followUp").is_some_and(|f| !f.is_null());
    let mut blocks = vec![];
    let mut lines: Vec<Option<String>> = vec![];
    let mut buttons: Vec<Value> = match (&prompt.decision, prompt.branch) {
        (None, _) => prompt
            .choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                button(format!("agentflow_answer_choice_{index}"), choice, &index.to_string(), false)
            })
            .collect(),
        (Some(decision), None) => {
            let choices = decision["choices"].as_array().map(Vec::as_slice).unwrap_or_default();
            let mut heading: Vec<String> =
                risk_label(text(decision, "risk")).map(|r| format!("*{r}*")).into_iter().collect();
            if !text(decision, "context").is_empty() {
                heading.push(escape(text(decision, "context")));
            }
            let mut legend = vec![];
            if choices.iter().any(recommended) {
                legend.push("Green = the agent's pick");
            }
            if choices.iter().any(has_follow_up) {
                legend.push("› asks a follow-up");
            }
            lines.push(Some(heading.join(" · ")));
            lines.push(Some(legend.join(" · ")));
            lines.extend(
                choices.iter().enumerate().map(|(index, choice)| choice_notes(&(index + 1).to_string(), choice)),
            );
            choices
                .iter()
                .enumerate()
                .map(|(index, choice)| {
                    let suffix = if has_follow_up(choice) { " ›" } else { "" };
                    let label = format!("{} · {}{suffix}", index + 1, text(choice, "label"));
                    button(format!("agentflow_answer_choice_{index}"), &label, &index.to_string(), recommended(choice))
                })
                .collect()
        }
        (Some(decision), Some(branch)) => {
            let chosen = &decision["choices"][branch];
            let follow_up = &chosen["followUp"];
            let picked = format!(
                "You picked *{}*, one more choice:\n*{}*",
                escape(text(chosen, "label")),
                escape(text(follow_up, "message"))
            );
            blocks.push(json!({"type":"section","block_id":FOLLOW_UP_BLOCK,"text":{"type":"mrkdwn","text":cut(&picked, SECTION_LIMIT)}}));
            let options = follow_up["choices"].as_array().map(Vec::as_slice).unwrap_or_default();
            lines.push(options.iter().any(recommended).then(|| "Green = the agent's pick".to_string()));
            lines.extend(
                options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| choice_notes(&format!("{}.{}", branch + 1, index + 1), option)),
            );
            let mut buttons: Vec<Value> = options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    let label = format!("{}.{} · {}", branch + 1, index + 1, text(option, "label"));
                    let value = format!("{branch}.{index}");
                    button(format!("agentflow_answer_follow_{index}"), &label, &value, recommended(option))
                })
                .collect();
            buttons.push(button("agentflow_answer_back".into(), "‹ Change branch", "back", false));
            buttons
        }
    };
    buttons.push(json!({"type":"button","action_id":"agentflow_answer_reply","text":plain("Reply…",75),"value":format!("{reference}|reply")}));
    let context: Vec<Value> = lines
        .into_iter()
        .flatten()
        .filter(|line| !line.is_empty())
        .take(CONTEXT_ELEMENTS)
        .map(|line| json!({"type":"mrkdwn","text":cut(&line, SECTION_LIMIT)}))
        .collect();
    if !context.is_empty() {
        blocks.push(json!({"type":"context","block_id":"notes","elements":context}));
    }
    blocks.push(json!({"type":"actions","block_id":"answer","elements":buttons}));
    blocks
}
/// A follow-up option's `branch.option` from a button value.
pub fn parse_path(choice: &str) -> Option<(usize, usize)> {
    let (branch, option) = choice.split_once('.')?;
    let number = |part: &str| {
        (!part.is_empty() && part.len() <= 3 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    Some((number(branch)?, number(option)?))
}
pub fn offer_blocks(offer: &AnswerOffer) -> Vec<Value> {
    let value = format!("{}|{}|{}", offer.jira_key, offer.question_uid, offer.text);
    if value.chars().count() > BUTTON_VALUE_LIMIT {
        return sections("That is too long to send from here; use Reply… on the question, or answer in AgentFlow.");
    }
    let mut lines = vec!["Send this to the agent as your answer?".into()];
    lines.extend(quote(&offer.text));
    let mut blocks = sections(&lines.join("\n"));
    blocks.push(json!({"type":"actions","block_id":"offer","elements":[{"type":"button","action_id":"agentflow_answer_typed","style":"primary","text":plain("Send as answer",75),"value":value}]}));
    blocks
}
pub fn buttons_block(buttons: &[Button]) -> Value {
    let elements: Vec<_> = buttons.iter().enumerate().map(|(index, button)| {
        let mut value = json!({"type":"button","action_id":format!("agentflow_{}_{index}",button.kind),"text":plain(&button.label,75)});
        if !button.value.is_empty() { value["value"] = button.value.chars().take(BUTTON_VALUE_LIMIT).collect::<String>().into(); }
        match button.kind.as_str() { "run" | "approve_requirements" => value["style"] = "primary".into(), "cancel" => value["style"] = "danger".into(), _ => {} }
        if let Some(confirm) = button.confirm.as_deref().filter(|s| !s.is_empty()) {
            value["confirm"] = json!({"title":plain("Are you sure?",100),"text":plain(confirm,300),"confirm":plain("Yes",30),"deny":plain("Cancel",30)});
        }
        value
    }).collect();
    json!({"type":"actions","block_id":"buttons","elements":elements})
}
pub fn reply_blocks(reply: &Reply) -> Option<Vec<Value>> {
    if reply.buttons.is_empty() && reply.offer.is_none() {
        return (reply.text.chars().count() > SECTION_LIMIT).then(|| sections(&reply.text));
    }
    let mut blocks = sections(&reply.text);
    if !reply.buttons.is_empty() {
        blocks.push(buttons_block(&reply.buttons));
    }
    if let Some(offer) = &reply.offer {
        blocks.extend(offer_blocks(offer));
    }
    Some(blocks)
}
pub fn option(value: &str, label: &str) -> Option<Value> {
    if value.is_empty() || value.chars().count() > 150 {
        return None;
    }
    let chars: Vec<_> = label.chars().collect();
    let label = if chars.len() > 75 {
        format!("…{}", chars[chars.len() - 74..].iter().collect::<String>())
    } else {
        label.into()
    };
    Some(json!({"text":plain(&label,75),"value":value}))
}
pub fn parse_offer(value: &str) -> Option<(&str, &str, &str)> {
    let mut parts = value.splitn(3, '|');
    let result = (parts.next()?, parts.next()?, parts.next()?);
    (!result.0.is_empty() && !result.1.is_empty() && !result.2.is_empty()).then_some(result)
}
pub fn parse_reference(value: &str) -> Option<(&str, &str, &str)> {
    let result = parse_offer(value)?;
    (!result.2.contains('|')).then_some(result)
}
pub(crate) fn needs_you(item: &Value) -> String {
    let questions = item["questions"].as_u64().unwrap_or(0);
    let label = text(item, "label");
    let suffix = if questions == 0 {
        String::new()
    } else {
        format!(" ({questions} question{})", if questions == 1 { "" } else { "s" })
    };
    format!(
        "{}: {}{suffix}",
        escape(if label.is_empty() { text(item, "nodeKey") } else { label }),
        escape(text(item, "action"))
    )
}
pub fn render_status(snapshot: &Value, url: &str) -> String {
    let mut lines = vec![heading(snapshot), format!("Phase: {}", phase(snapshot))];
    if let Some(attention) = snapshot["attention"].as_array().filter(|a| !a.is_empty()) {
        lines.push("Needs you:".into());
        lines.extend(attention.iter().map(|item| format!("• {}", needs_you(item))));
    }
    lines.push(format!("<{url}|Open in AgentFlow>"));
    lines.join("\n")
}
pub fn render_list(tasks: &[Value]) -> String {
    let live: Vec<_> = tasks.iter().filter(|t| !matches!(text(t, "status"), "MERGED" | "CANCELLED")).collect();
    if live.is_empty() {
        return "No active AgentFlow tasks.".into();
    }
    let mut lines = vec![];
    for task in live.iter().take(25) {
        let mut line = format!("• {} · {}", heading(task), phase(task));
        if let Some(item) = task["attention"].as_array().and_then(|a| a.first()) {
            line.push_str(&format!(" · needs you: {}", needs_you(item)));
        }
        lines.push(line);
    }
    if live.len() > 25 {
        lines.push(format!("…and {} more", live.len() - 25));
    }
    lines.join("\n")
}
