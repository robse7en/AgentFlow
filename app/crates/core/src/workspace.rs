//! Task workspace layout under `<taskDirectory>/<JIRA-KEY>/`, ported from
//! `backend/agentflow/services/workspace.py`. Files are written exactly as Python writes them
//! (see [`crate::pyfile`]), since agents and the Python backend read them.

use std::io;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::{Value, json};

use crate::db::DbTime;
use crate::pyfile;

pub struct TaskWorkspace {
    pub root: PathBuf,
    pub context_dir: PathBuf,
    pub attachments_dir: PathBuf,
    pub requirements_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub findings_dir: PathBuf,
    pub final_dir: PathBuf,
}

/// Requirement document keys (as the API names them) and their files, in display order.
pub const REQUIREMENT_FILES: [(&str, &str); 5] = [
    ("requirements", "requirements.md"),
    ("acceptanceCriteria", "acceptance-criteria.md"),
    ("assumptions", "assumptions.md"),
    ("implementationPlan", "implementation-plan.md"),
    ("risks", "risks.md"),
];

impl TaskWorkspace {
    pub fn new(task_root: &Path, jira_key: &str) -> TaskWorkspace {
        let root = task_root.join(jira_key);
        let context_dir = root.join("context");
        TaskWorkspace {
            attachments_dir: context_dir.join("attachments"),
            requirements_dir: root.join("requirements"),
            runs_dir: root.join("runs"),
            findings_dir: root.join("findings"),
            final_dir: root.join("final"),
            context_dir,
            root,
        }
    }

    pub fn create(&self, task_meta: &Value, additional_context: Option<&str>) -> io::Result<()> {
        for dir in
            [&self.root, &self.context_dir, &self.requirements_dir, &self.runs_dir, &self.findings_dir, &self.final_dir]
        {
            std::fs::create_dir_all(dir)?;
        }
        write_json(&self.root.join("task.json"), task_meta)?;
        pyfile::write_text(
            &self.context_dir.join("additional-context.md"),
            &format!("{}\n", additional_context.unwrap_or_default().trim()),
        )
    }

    /// `runs/<role_dir>/<NN>`, created.
    pub fn run_dir(&self, role_dir: &str, run_number: u32) -> io::Result<PathBuf> {
        let dir = self.runs_dir.join(role_dir).join(format!("{run_number:02}"));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// A file under the task root, or `default` when it does not exist.
    pub fn read_text(&self, relative: &str, default: &str) -> io::Result<String> {
        let path = self.root.join(relative);
        if path.exists() { pyfile::read_text(&path) } else { Ok(default.to_string()) }
    }

    pub fn requirement_files(&self) -> io::Result<IndexMap<String, String>> {
        REQUIREMENT_FILES
            .iter()
            .map(|(key, name)| Ok((key.to_string(), self.read_text(&format!("requirements/{name}"), "")?)))
            .collect()
    }

    /// The last synced copy of the Jira ticket (jira-issue.md) and when it was synced, or None
    /// before the first sync.
    pub fn jira_issue(&self) -> io::Result<Option<Value>> {
        let path = self.context_dir.join("jira-issue.md");
        if !path.is_file() {
            return Ok(None);
        }
        let synced_at = read_json(&self.context_dir.join("jira-manifest.json"))
            .ok()
            .and_then(|manifest| manifest.get("syncedAt").cloned())
            .unwrap_or(Value::Null);
        Ok(Some(json!({"markdown": pyfile::read_text(&path)?, "syncedAt": synced_at})))
    }

    /// Past "Request changes" rounds on the requirements gate (feedback-<N>.md), oldest first.
    pub fn requirements_feedback(&self) -> io::Result<Vec<Value>> {
        if !self.requirements_dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.requirements_dir)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("feedback-") && n.ends_with(".md"))
            })
            .collect();
        files.sort();
        let mut entries = Vec::new();
        for path in files {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            let Ok(iteration) = stem.rsplit('-').next().unwrap_or_default().parse::<i64>() else { continue };
            let modified: chrono::DateTime<chrono::Utc> = std::fs::metadata(&path)?.modified()?.into();
            entries.push(json!({
                "iteration": iteration,
                "feedback": pyfile::read_text(&path)?.trim(),
                "at": DbTime(modified.naive_utc()).isoformat(),
            }));
        }
        Ok(entries)
    }
}

/// Create a run credential without a window where it is readable by others, and refuse to
/// overwrite one. Compact JSON, as Python's `json.dump` writes it (both flowctls read it).
pub fn write_credential(path: &Path, data: &Value) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(pyjson::dumps_compact(data).as_bytes())
}

/// `json.dumps(data, indent=2) + "\n"` written as Python's write_text would.
pub fn write_json(path: &Path, data: &Value) -> io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    pyfile::write_text(path, &(pyjson::dumps(data) + "\n"))?;
    Ok(path.to_path_buf())
}

pub fn read_json(path: &Path) -> io::Result<Value> {
    serde_json::from_str(&pyfile::read_text(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
