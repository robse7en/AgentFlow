//! Plain-text Jira context, with inline pointers to local attachments.
use regex::{Captures, Regex};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn sanitize_filename(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("");
    let safe = Regex::new(r"[^A-Za-z0-9 ._-]+").unwrap().replace_all(name, "_");
    let safe = safe.trim_matches([' ', '.', '_']);
    if safe.is_empty() { "attachment".into() } else { safe.chars().take(200).collect() }
}
use super::entities::unescape;
fn attachment_id(markup: &str, names: &BTreeMap<String, String>) -> Option<String> {
    for pattern in [
        r"(?i)/attachment/content/([0-9]{1,30})",
        r"(?i)/attachment/thumbnail/([0-9]{1,30})",
        r"(?i)/secure/attachment/([0-9]{1,30})/",
        r"(?i)/secure/thumbnail/([0-9]{1,30})/",
        r#"(?i)data-attachment-id=["']([0-9]{1,30})["']"#,
    ] {
        if let Some(c) = Regex::new(pattern).unwrap().captures(markup) {
            return Some(c[1].into());
        }
    }
    Regex::new(r#"(?i)(?:data-attachment-name|alt)=["']([^"']+)["']"#)
        .unwrap()
        .captures(markup)
        .and_then(|c| names.get(&c[1]).cloned())
}
pub fn referenced_ids(text: &str, names: &BTreeMap<String, String>) -> Vec<String> {
    let mut ids = Vec::new();
    for tag in Regex::new(r"<[^>]+>").unwrap().find_iter(&unescape(text)) {
        if let Some(id) = attachment_id(tag.as_str(), names)
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}
fn marker(id: &str, entries: &BTreeMap<String, Value>) -> String {
    let Some(entry) = entries.get(id).filter(|e| e["status"] != "removed") else {
        return format!("[attachment {id}: not an attachment of this ticket]");
    };
    let name = entry["filename"].as_str().unwrap_or("None");
    if entry["status"] != "present" {
        return format!("[attachment {id} ({name}): not downloaded - {}]", entry["reason"].as_str().unwrap_or("None"));
    }
    let file = entry["file"].as_str().unwrap_or("");
    let path = format!("attachments/{file}");
    let mime = entry["mimeType"].as_str();
    if crate::media::is_video(file, mime) {
        if entry["frames"].as_array().is_some_and(|a| !a.is_empty()) {
            return format!("[video: {path} - keyframes in frames/{id}/]");
        }
        return format!("[video: {path}]");
    }
    let extension = std::path::Path::new(file).extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    if mime.is_some_and(|m| m.starts_with("image/"))
        || ["png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&extension.as_str())
    {
        return format!("[image: {path}]");
    }
    format!("[attachment: {path}]")
}
pub fn html_to_text(text: &str, entries: &BTreeMap<String, Value>, names: &BTreeMap<String, String>) -> String {
    let text = unescape(text);
    let text = Regex::new(r"(?is)<a\b[^>]*>.*?</a>").unwrap().replace_all(&text, |c: &Captures<'_>| {
        attachment_id(&c[0], names).map(|id| marker(&id, entries)).unwrap_or_else(|| c[0].into())
    });
    let text = Regex::new(r"<[^>]+>").unwrap().replace_all(&text, |c: &Captures<'_>| {
        if let Some(id) = attachment_id(&c[0], names) {
            return marker(&id, entries);
        }
        if c[0].to_lowercase().starts_with("<img")
            && let Some(src) = Regex::new(r#"src=["']([^"']+)["']"#).unwrap().captures(&c[0])
        {
            return format!("[image not attached to the ticket: {}]", &src[1]);
        }
        c[0].into()
    });
    let text = Regex::new(r"(?i)<br\s*/?>|</(?:p|li|h[1-6]|div|ul|ol)>").unwrap().replace_all(&text, "\n");
    let text = Regex::new(r"<[^>]+>").unwrap().replace_all(&text, "");
    Regex::new(r"\n{3,}").unwrap().replace_all(&text, "\n\n").trim().into()
}
pub fn issue_markdown(
    key: &str,
    fields: &Value,
    rendered: &Value,
    comments: Option<&[Value]>,
    entries: &BTreeMap<String, Value>,
    error: Option<&str>,
) -> String {
    let names = fields["attachment"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["filename"].as_str()?.into(), a["id"].as_str()?.into())))
        .collect();
    let description = html_to_text(rendered["description"].as_str().unwrap_or(""), entries, &names);
    let mut parts = vec![
        format!("# {key}: {}", fields["summary"].as_str().unwrap_or("")),
        "## Description".into(),
        if description.is_empty() { "(no description)".into() } else { description },
    ];
    if let Some(error) = error {
        parts.extend(["## Comments".into(), format!("Not fetched ({error}) - read them through your Jira MCP.")]);
    } else if let Some(comments) = comments {
        parts.push(format!("## Comments ({})", comments.len()));
        for comment in comments {
            let author = comment["author"]["displayName"].as_str().filter(|s| !s.is_empty()).unwrap_or("unknown");
            let created =
                comment["created"].as_str().unwrap_or("").chars().take(16).collect::<String>().replace('T', " ");
            let body = html_to_text(comment["renderedBody"].as_str().unwrap_or(""), entries, &names);
            parts.extend([
                format!("### Comment {} - {author}, {created}", comment["id"].as_str().unwrap_or("None")),
                if body.is_empty() { "(empty)".into() } else { body },
            ]);
        }
        if comments.is_empty() {
            parts.push("(none)".into());
        }
    }
    parts.join("\n\n") + "\n"
}
