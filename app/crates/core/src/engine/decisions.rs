//! Decision-tree agent questions, ported from `workflow/decisions.py` message for message: what
//! `flowctl question --file` may send, and the one line the agent gets back.
//!
//! A question is flat (text choices, the answer typed back as is) or a decision tree (object
//! choices, an optional `context` and `risk`, at most one level of follow-up questions). A tree is
//! stored as validated in `agent_questions.decision`; its top-level labels also go into `choices`.
//! The human answers with a path and the line is built from the stored tree:
//! `2: label`, `1.1: branch → option`, or `other: free text`.

use serde_json::{Map, Value, json};

use super::agents::py_isprintable;
use super::prompts::py_strip;

/// Between a branch and its follow-up option in an answer line. Labels may not contain its glyph.
pub const ANSWER_SEPARATOR: &str = " \u{2192} ";
pub const OTHER_PREFIX: &str = "other: ";

const RISKS: [&str; 3] = ["low", "medium", "high"];
const MAX_CHOICES: usize = 8;
const MIN_FOLLOW_UP_CHOICES: usize = 2;
const LABEL_LIMIT: usize = 250;
const CONTEXT_LIMIT: usize = 500;
const FOLLOW_UP_MESSAGE_LIMIT: usize = 500;
const CHOICE_TEXT_LIMITS: [(&str, usize); 4] = [("detail", 200), ("warning", 120), ("note", 300), ("source", 200)];
const CHOICE_FIELDS: [&str; 7] = ["label", "recommended", "detail", "warning", "note", "source", "followUp"];
const FOLLOW_UP_FIELDS: [&str; 2] = ["message", "choices"];

pub const NO_SUCH_CHOICE: &str = "That choice does not exist";
pub const NEEDS_FOLLOW_UP: &str = "That choice has a follow-up question; pick one of its options";

/// One printable line of at most `limit` characters after trimming; blank counts as absent.
fn text(value: Option<&Value>, field: &str, limit: usize, required: bool) -> Result<Option<String>, String> {
    let value = match value {
        None | Some(Value::Null) if required => return Err(format!("{field}: field required")),
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(value)) => py_strip(value),
        Some(_) => return Err(format!("{field}: must be a string")),
    };
    if value.is_empty() {
        return if required { Err(format!("{field}: cannot be empty")) } else { Ok(None) };
    }
    if value.chars().count() > limit {
        return Err(format!("{field}: must be at most {limit} characters"));
    }
    if !value.chars().all(py_isprintable) {
        return Err(format!("{field}: must be a single line"));
    }
    Ok(Some(value.to_string()))
}

/// A repository-relative path (no `..` segment, drive letter, scheme or leading slash) or an https
/// URL. It is only displayed, never fetched or opened by AgentFlow itself.
pub fn source_allowed(source: &str) -> bool {
    if let Some(rest) = source.strip_prefix("https://") {
        return !rest.is_empty() && !source.chars().any(char::is_whitespace);
    }
    if source.starts_with(['/', '\\']) || source.contains(':') {
        return false;
    }
    !source.split(['/', '\\']).any(|segment| segment == "..")
}

fn unknown(raw: &Map<String, Value>, allowed: &[&str], field: &str) -> Result<(), String> {
    let mut unknown: Vec<&String> = raw.keys().filter(|key| !allowed.contains(&key.as_str())).collect();
    unknown.sort();
    match unknown.first() {
        Some(key) => Err(format!("{field}: unknown field '{key}'")),
        None => Ok(()),
    }
}

fn choice(raw: &Value, field: &str, top_level: bool) -> Result<Value, String> {
    let Some(raw) = raw.as_object() else { return Err(format!("{field}: must be an object")) };
    unknown(raw, &CHOICE_FIELDS, field)?;
    let label = text(raw.get("label"), &format!("{field}.label"), LABEL_LIMIT, true)?.unwrap_or_default();
    let glyph = ANSWER_SEPARATOR.trim();
    if label.contains(glyph) {
        return Err(format!("{field}.label: cannot contain {glyph}"));
    }
    let recommended = match raw.get("recommended") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(format!("{field}.recommended: must be true or false")),
    };
    let mut choice = Map::new();
    choice.insert("label".into(), json!(label));
    if recommended {
        choice.insert("recommended".into(), json!(true));
    }
    for (key, limit) in CHOICE_TEXT_LIMITS {
        if let Some(value) = text(raw.get(key), &format!("{field}.{key}"), limit, false)? {
            choice.insert(key.into(), json!(value));
        }
    }
    if let Some(source) = choice.get("source").and_then(Value::as_str) {
        if !choice.contains_key("note") {
            return Err(format!("{field}.source: needs a note"));
        }
        if !source_allowed(source) {
            return Err(format!(
                "{field}.source: must be a repository-relative path (no '..', drive letter or leading /) or an https:// URL"
            ));
        }
    }
    match raw.get("followUp") {
        None | Some(Value::Null) => {}
        Some(_) if !top_level => {
            return Err(format!("{field}.followUp: follow-up options cannot ask another follow-up (one level only)"));
        }
        Some(follow_up) => {
            choice.insert("followUp".into(), self::follow_up(follow_up, &format!("{field}.followUp"))?);
        }
    }
    Ok(Value::Object(choice))
}

fn level(raw: Option<&Value>, field: &str, minimum: usize, top_level: bool) -> Result<Vec<Value>, String> {
    let Some(Value::Array(items)) = raw else { return Err(format!("{field}: must be a list")) };
    if !(minimum..=MAX_CHOICES).contains(&items.len()) {
        return Err(format!("{field}: must have {minimum} to {MAX_CHOICES} choices"));
    }
    let choices = items
        .iter()
        .enumerate()
        .map(|(index, item)| choice(item, &format!("{field}[{index}]"), top_level))
        .collect::<Result<Vec<_>, _>>()?;
    // Python compares casefold()ed labels; lower case is the same for everything but a few
    // special cases (ß).
    let mut folded: Vec<String> = choices.iter().map(|c| c["label"].as_str().unwrap_or("").to_lowercase()).collect();
    folded.sort();
    folded.dedup();
    if folded.len() != choices.len() {
        return Err(format!("{field}: labels must be unique"));
    }
    if choices.iter().filter(|c| c.get("recommended").is_some()).count() > 1 {
        return Err(format!("{field}: at most one choice can be recommended"));
    }
    Ok(choices)
}

fn follow_up(raw: &Value, field: &str) -> Result<Value, String> {
    let Some(raw) = raw.as_object() else { return Err(format!("{field}: must be an object")) };
    unknown(raw, &FOLLOW_UP_FIELDS, field)?;
    let message = text(raw.get("message"), &format!("{field}.message"), FOLLOW_UP_MESSAGE_LIMIT, true)?;
    let choices = level(raw.get("choices"), &format!("{field}.choices"), MIN_FOLLOW_UP_CHOICES, false)?;
    Ok(json!({"message": message, "choices": choices}))
}

fn present(value: Option<&Value>) -> bool {
    value.is_some_and(|v| !v.is_null())
}

/// Object choices, a context or a risk make a decision tree; text choices alone a flat question.
pub fn is_tree(choices: &[Value], context: Option<&Value>, risk: Option<&Value>) -> bool {
    present(context) || present(risk) || choices.iter().any(Value::is_object)
}

/// The stored tree `{context, risk, choices}`, or the problem naming the field at fault.
pub fn clean_decision(choices: &[Value], context: Option<&Value>, risk: Option<&Value>) -> Result<Value, String> {
    if !choices.iter().all(Value::is_object) {
        return Err(
            "choices: use objects for every choice of a decision tree (or only text choices for a flat question)"
                .into(),
        );
    }
    let risk = match risk {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(risk)) if RISKS.contains(&risk.as_str()) => json!(risk),
        Some(_) => return Err("risk: must be low, medium or high".into()),
    };
    let context = text(context, "context", CONTEXT_LIMIT, false)?;
    let choices = level(Some(&Value::Array(choices.to_vec())), "choices", 0, true)?;
    Ok(json!({"context": context, "risk": risk, "choices": choices}))
}

/// What is typed into the agent's pane for a path (0-based indexes) or free text. Flat questions
/// get the choice text or the free text unchanged; the caller still applies `clean_answer`.
pub fn answer_line(
    choices: &[String],
    decision: Option<&Value>,
    path: Option<&[usize]>,
    free_text: Option<&str>,
) -> Result<String, String> {
    let Some(path) = path else {
        let free_text = free_text.unwrap_or("");
        return Ok(if decision.is_some() { format!("{OTHER_PREFIX}{free_text}") } else { free_text.to_string() });
    };
    let Some(decision) = decision else {
        return match path {
            [index] if *index < choices.len() => Ok(choices[*index].clone()),
            _ => Err(NO_SUCH_CHOICE.into()),
        };
    };
    let level = decision["choices"].as_array().map(Vec::as_slice).unwrap_or_default();
    let (first, rest) = match path {
        [first, rest @ ..] if rest.len() <= 1 && *first < level.len() => (*first, rest),
        _ => return Err(NO_SUCH_CHOICE.into()),
    };
    let branch = &level[first];
    let label = branch["label"].as_str().unwrap_or("");
    let follow_up = branch.get("followUp").filter(|f| !f.is_null());
    match (rest, follow_up) {
        ([], Some(_)) => Err(NEEDS_FOLLOW_UP.into()),
        ([], None) => Ok(format!("{}: {label}", first + 1)),
        ([second], Some(follow_up)) => {
            let options = follow_up["choices"].as_array().map(Vec::as_slice).unwrap_or_default();
            let option = options.get(*second).ok_or_else(|| NO_SUCH_CHOICE.to_string())?;
            let option = option["label"].as_str().unwrap_or("");
            Ok(format!("{}.{}: {label}{ANSWER_SEPARATOR}{option}", first + 1, second + 1))
        }
        _ => Err(NO_SUCH_CHOICE.into()),
    }
}
