//! Claude Code specifics, ported from `backend/agentflow/harnesses/claude.py`.

use std::path::Path;

use serde_json::{Map, Value};

/// Pre-accept Claude Code's one-time-per-directory workspace trust dialog for `cwd`.
///
/// Claude Code asks "Do you trust the files in this folder?" the first time it's launched in a
/// directory it hasn't seen, tracked as `hasTrustDialogAccepted` per project path in
/// `~/.claude.json` (`config_path`). Every task runs in a fresh worktree, so without this the
/// dialog fires on first launch and, with no one attached to answer it, the process just sits
/// there. This writes exactly what answering "yes" once would.
///
/// `~/.claude.json` is the file the user's own Claude Code sessions read and write, so this reads
/// it fresh, changes only this one project's flag, and writes back atomically (temp file +
/// replace). Best-effort: a failure means the dialog may still appear, never a failed launch.
///
/// The file is written the way Python's `json.dump` writes it. Every value survives, but the text
/// is reformatted, as Python reformatted it; Claude Code rewrites the file in its own format later.
/// One difference from Python: an integer beyond 64 bits becomes the same double in float
/// notation (Python kept its digits). Claude Code (Node) reads it as that double either way.
pub fn trust_workspace(config_path: &Path, cwd: &Path) {
    if !config_path.is_file() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(config_path) else { return };
    // A leading UTF-8 BOM makes this fail, as it makes Python's json.load fail.
    let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(&text) else { return };
    let projects = data.entry("projects").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(projects) = projects else { return };
    let key = super::as_posix(cwd);
    let entry = projects.get(&key);
    if let Some(Value::Object(entry)) = entry
        && entry.get("hasTrustDialogAccepted") == Some(&Value::Bool(true))
    {
        return;
    }
    let mut entry = match entry {
        Some(Value::Object(entry)) => entry.clone(),
        _ => Map::new(),
    };
    entry.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    projects.insert(key, Value::Object(entry));
    let tmp = config_path.with_file_name(format!(
        "{}.agentflow-tmp",
        config_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    if std::fs::write(&tmp, pyjson::dumps_compact(&Value::Object(data))).is_ok() {
        // std's rename replaces an existing file on Windows too, like Python's Path.replace.
        let _ = std::fs::rename(&tmp, config_path);
    }
}
