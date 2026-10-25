//! The Jira ticket data AgentFlow syncs into `<task>/context/`, ported from
//! `backend/agentflow/services/jira_client.py`: REST, attachment manifests, rendered context,
//! local video keyframes and the generation an Architect was shown.

use std::path::Path;

use serde_json::{Map, Value, json};

mod client;
mod entities;
mod render;
mod sync;
pub use client::{HttpReply, JiraError, JiraService, JiraTransport, UreqTransport, build_service};
pub use render::{html_to_text, issue_markdown, referenced_ids, sanitize_filename};
pub use sync::{SkippedAttachment, TicketSync};

fn read_manifest(path: &Path) -> Map<String, Value> {
    match crate::pyfile::read_text(path).ok().and_then(|text| serde_json::from_str(&text).ok()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Python's `isinstance(value, int)`, which a bool also passes.
fn python_int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// The sync generation an Architect prompt is being built from, or None before any sync.
pub fn manifest_generation(context_dir: &Path) -> Option<i64> {
    python_int(read_manifest(&context_dir.join("jira-manifest.json")).get("syncGeneration"))
}

/// Record that a launched Architect was shown this sync generation. Best-effort: a missed mark
/// only means the next prompt re-flags some changes as new.
pub fn mark_prompted(context_dir: &Path, generation: i64) {
    let path = context_dir.join("jira-manifest.json");
    let mut manifest = read_manifest(&path);
    if manifest.is_empty() {
        return;
    }
    let last = match manifest.get("lastPromptedGeneration") {
        None | Some(Value::Null) => 0,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::Bool(b)) => i64::from(*b),
        // Python's max() would raise on any other type; the mark is best-effort.
        Some(_) => return,
    };
    manifest.insert("lastPromptedGeneration".into(), json!(generation.max(last)));
    let tmp = path.with_file_name(format!("jira-manifest.json.{}.tmp", crate::api::auth::token_hex(4)));
    let text = pyjson::dumps(&Value::Object(manifest)) + "\n";
    if crate::pyfile::write_text(&tmp, &text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marking_keeps_the_highest_generation() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(manifest_generation(dir.path()), None);
        mark_prompted(dir.path(), 3); // no manifest: nothing to mark
        assert!(!dir.path().join("jira-manifest.json").exists());
        let manifest = dir.path().join("jira-manifest.json");
        std::fs::write(&manifest, r#"{"syncGeneration": 4, "lastPromptedGeneration": 5, "attachments": []}"#).unwrap();
        assert_eq!(manifest_generation(dir.path()), Some(4));
        mark_prompted(dir.path(), 4);
        let written: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(written["lastPromptedGeneration"], 5);
        mark_prompted(dir.path(), 7);
        let written: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(written["lastPromptedGeneration"], 7);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no temp file left behind");
    }
}
