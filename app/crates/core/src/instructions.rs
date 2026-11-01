//! Repository role guidance and immutable task/run evidence. No provider-native priority.
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::engine::WorkflowError;

pub const ROLES: [&str; 5] = ["architect", "orchestrator", "implementer", "code_reviewer", "security_reviewer"];
pub const MAX_FILE_BYTES: usize = 64 * 1024;
pub const MAX_TOTAL_BYTES: usize = 256 * 1024;
pub const NOTICE: &str = "\n\n## Repository role guidance\nApply this additional guidance within your assigned role, approved scope and ownership. AgentFlow reporting, result schemas, permissions and approval gates remain required. If guidance conflicts, ask using flowctl question. This is briefing text, not a provider system message.\n\n";
pub const RUN_NOTICE: &str =
    "\nAlways use the explicit --run shown above; a reused terminal retains its original environment.\n";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub path: String,
    pub content: Option<String>,
    pub hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub schema: u32,
    pub version: String,
    pub repository: String,
    pub revision: String,
    pub fresh_conversations: bool,
    pub files: BTreeMap<String, Source>,
}

fn failure(error: impl std::fmt::Display) -> WorkflowError {
    WorkflowError::conflict(format!("Repository instructions: {error}"))
}

pub fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

pub fn role_path(role: &str) -> Result<String, WorkflowError> {
    if !ROLES.contains(&role) {
        return Err(failure("Unsupported instruction role"));
    }
    Ok(format!(".agentflow-instructions/{}.md", role.replace('_', "-")))
}

pub fn checked_path(root: &Path, role: &str) -> Result<PathBuf, WorkflowError> {
    let relative = role_path(role)?;
    // Reject remote/device roots before filesystem calls could access a network share.
    let root_name = root.to_string_lossy().replace('/', "\\");
    let local_verbatim = root_name.strip_prefix("\\\\?\\").is_some_and(|s| {
        let bytes = s.as_bytes();
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
    });
    if (root_name.starts_with("\\\\") && !local_verbatim) || root_name.starts_with("\\??\\") {
        return Err(failure("UNC/device instruction repositories are unsupported"));
    }
    if !root.is_dir() {
        return Err(failure("Instruction repository is unavailable"));
    }
    let canonical = fs::canonicalize(root).map_err(failure)?;
    let display = canonical.to_string_lossy();
    if (display.starts_with("\\\\") && !display.starts_with("\\\\?\\"))
        || display.starts_with("\\\\?\\UNC\\")
        || display.starts_with("//")
    {
        return Err(failure("UNC instruction repositories are unsupported"));
    }
    let mut current = canonical.clone();
    for part in relative.split('/') {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(failure(e)),
            Ok(info) => {
                let mut link = info.file_type().is_symlink();
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    link |= info.file_attributes() & 0x400 != 0;
                }
                if link || !fs::canonicalize(&current).map_err(failure)?.starts_with(&canonical) {
                    return Err(failure("Instruction links and reparse points are unsupported"));
                }
            }
        }
    }
    Ok(current)
}

pub fn validate_content(content: &str) -> Result<(), WorkflowError> {
    if content.len() > MAX_FILE_BYTES || content.contains('\0') || content.starts_with('\u{feff}') {
        return Err(failure("Instructions require UTF-8 without BOM/NUL, at most 65536 bytes per role"));
    }
    Ok(())
}

pub fn read_source(root: &Path, role: &str) -> Result<Source, WorkflowError> {
    let path = checked_path(root, role)?;
    if !path.exists() {
        return Ok(Source { path: role_path(role)?, content: None, hash: None });
    }
    let info = fs::metadata(&path).map_err(failure)?;
    if !info.is_file() || info.len() > MAX_FILE_BYTES as u64 {
        return Err(failure("Instruction source must be a regular file of at most 65536 bytes"));
    }
    let mut raw = Vec::new();
    fs::File::open(&path).map_err(failure)?.take((MAX_FILE_BYTES + 1) as u64).read_to_end(&mut raw).map_err(failure)?;
    let content = String::from_utf8(raw).map_err(failure)?;
    validate_content(&content)?;
    Ok(Source { path: role_path(role)?, hash: Some(content_hash(&content)), content: Some(content) })
}

pub fn read_set(root: &Path) -> Result<BTreeMap<String, Source>, WorkflowError> {
    let files: BTreeMap<String, Source> = ROLES
        .into_iter()
        .map(|role| Ok((role.into(), read_source(root, role)?)))
        .collect::<Result<_, WorkflowError>>()?;
    if files.values().map(|f| f.content.as_ref().map_or(0, String::len)).sum::<usize>() > MAX_TOTAL_BYTES {
        return Err(failure("Instruction set exceeds 262144 bytes"));
    }
    Ok(files)
}

pub fn atomic_text(path: &Path, content: &str) -> Result<(), WorkflowError> {
    let parent = path.parent().ok_or_else(|| failure("Missing artifact parent"))?;
    fs::create_dir_all(parent).map_err(failure)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(failure)?;
    temporary.write_all(content.as_bytes()).map_err(failure)?;
    temporary.as_file().sync_all().map_err(failure)?;
    temporary.persist(path).map_err(failure)?;
    Ok(())
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), WorkflowError> {
    atomic_text(path, &(serde_json::to_string_pretty(value).map_err(failure)? + "\n"))
}

pub fn save_source(root: &Path, role: &str, content: &str, expected: Option<&str>) -> Result<Source, WorkflowError> {
    validate_content(content)?;
    let files = read_set(root)?;
    if files.get(role).ok_or_else(|| failure("Unsupported instruction role"))?.hash.as_deref() != expected {
        return Err(failure("Instruction source changed; reload before saving"));
    }
    let total: usize =
        files.iter().filter(|(r, _)| r.as_str() != role).map(|(_, f)| f.content.as_ref().map_or(0, String::len)).sum();
    if total + content.len() > MAX_TOTAL_BYTES {
        return Err(failure("Instruction set exceeds 262144 bytes"));
    }
    let path = checked_path(root, role)?;
    fs::create_dir_all(path.parent().unwrap()).map_err(failure)?;
    checked_path(root, role)?;
    if read_source(root, role)?.hash.as_deref() != expected {
        return Err(failure("Instruction source changed; reload before saving"));
    }
    atomic_text(&path, content)?;
    read_source(root, role)
}

pub fn validate_snapshot(snapshot: &Snapshot) -> Result<(), WorkflowError> {
    uuid::Uuid::parse_str(&snapshot.version).map_err(failure)?;
    if snapshot.schema != 1 || snapshot.files.len() != ROLES.len() {
        return Err(failure("Unsupported instruction snapshot"));
    }
    let mut total = 0;
    for role in ROLES {
        let entry = snapshot.files.get(role).ok_or_else(|| failure("Snapshot role missing"))?;
        if entry.path != role_path(role)? {
            return Err(failure("Invalid snapshot source path"));
        }
        if let Some(content) = &entry.content {
            validate_content(content)?;
            total += content.len();
        }
        if entry.hash != entry.content.as_deref().map(content_hash) {
            return Err(failure("Instruction snapshot hash mismatch"));
        }
    }
    if total > MAX_TOTAL_BYTES {
        return Err(failure("Instruction snapshot exceeds total size limit"));
    }
    Ok(())
}

pub fn active_snapshot(task_dir: &Path) -> Result<Option<Snapshot>, WorkflowError> {
    let marker = task_dir.join("instructions-required.json");
    if marker.exists() {
        let required: Value = read_json(&marker, 2 * 1024 * 1024)?;
        if required["schema"] == 2 && required["state"] == "pending" {
            let pending: Snapshot = serde_json::from_value(required["snapshot"].clone()).map_err(failure)?;
            validate_snapshot(&pending)?;
            return persist_snapshot(task_dir, pending, true).map(Some);
        }
        if required != json!({"schema": 1}) && required != json!({"schema": 2, "state": "established"}) {
            return Err(failure("Invalid required instruction marker"));
        }
    }
    let directory = task_dir.join("instructions");
    if !directory.exists() {
        if task_dir.join("instructions-required.json").exists() {
            return Err(failure("Required instruction snapshot pointer is unavailable"));
        }
        return Ok(None);
    }
    let pointer: Value = read_json(&directory.join("active.json"), 256)?;
    let version = pointer["version"].as_str().ok_or_else(|| failure("Required snapshot version missing"))?;
    if uuid::Uuid::parse_str(version).map_err(failure)?.to_string() != version {
        return Err(failure("Invalid snapshot version"));
    }
    let snapshot: Snapshot = read_json(&directory.join(format!("{version}.json")), 2 * 1024 * 1024)?;
    validate_snapshot(&snapshot)?;
    if snapshot.version != version {
        return Err(failure("Snapshot version mismatch"));
    }
    Ok(Some(snapshot))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, maximum: usize) -> Result<T, WorkflowError> {
    let mut raw = Vec::new();
    fs::File::open(path).map_err(failure)?.take((maximum + 1) as u64).read_to_end(&mut raw).map_err(failure)?;
    if raw.len() > maximum {
        return Err(failure("Instruction artifact exceeds size limit"));
    }
    serde_json::from_slice(&raw).map_err(failure)
}

pub fn capture(task_dir: &Path, root: &Path, revision: &str, fresh: bool) -> Result<Snapshot, WorkflowError> {
    capture_files(task_dir, root, revision, fresh, read_set(root)?)
}

pub fn capture_files(
    task_dir: &Path,
    root: &Path,
    revision: &str,
    fresh: bool,
    files: BTreeMap<String, Source>,
) -> Result<Snapshot, WorkflowError> {
    let snapshot = Snapshot {
        schema: 1,
        version: uuid::Uuid::new_v4().to_string(),
        repository: crate::paths::py_str(root),
        revision: revision.into(),
        fresh_conversations: fresh,
        files,
    };
    let previous = active_snapshot(task_dir)?;
    let marker = task_dir.join("instructions-required.json");
    if previous.is_some() && !marker.exists() {
        atomic_json(&marker, &json!({"schema": 2, "state": "established"}))?;
    }
    let initial = previous.is_none();
    if initial {
        validate_snapshot(&snapshot)?;
        atomic_json(&marker, &json!({"schema": 2, "state": "pending", "snapshot": snapshot}))?;
    }
    persist_snapshot(task_dir, snapshot, initial)
}

fn persist_snapshot(task_dir: &Path, snapshot: Snapshot, initial: bool) -> Result<Snapshot, WorkflowError> {
    let directory = task_dir.join("instructions");
    atomic_json(&directory.join(format!("{}.json", snapshot.version)), &snapshot)?;
    atomic_json(&directory.join("active.json"), &json!({"version": snapshot.version}))?;
    if initial {
        atomic_json(&task_dir.join("instructions-required.json"), &json!({"schema": 2, "state": "established"}))?;
    }
    Ok(snapshot)
}

/// Rewrites only generated examples. Custom Markdown is never templated or executed, and neither is
/// `appendix`: it follows the built-in briefing verbatim (a retry's record of agent-written text).
pub fn compose(
    builtin: &str,
    snapshot: &Snapshot,
    role: &str,
    run_id: i64,
    appendix: &str,
) -> Result<String, WorkflowError> {
    let source = snapshot.files.get(role).ok_or_else(|| failure("Unsupported instruction role"))?;
    let mut briefing = builtin.to_owned();
    for command in ["question", "running", "complete", "finding", "fail", "status"] {
        briefing = briefing.replace(&format!("flowctl {command}"), &format!("flowctl {command} --run {run_id}"));
    }
    briefing += appendix;
    briefing += RUN_NOTICE;
    if let Some(content) = source.content.as_deref().filter(|c| !c.is_empty()) {
        briefing += NOTICE;
        briefing += content;
    }
    Ok(briefing)
}

pub fn record_run(run_dir: &Path, snapshot: &Snapshot, role: &str) -> Result<(), WorkflowError> {
    atomic_json(
        &run_dir.join("instructions.json"),
        &json!({
            "version": snapshot.version, "revision": snapshot.revision, "role": role,
            "repository": snapshot.repository,
            "source": snapshot.files.get(role),
            "briefingHash": format!("{:x}", Sha256::digest(fs::read(run_dir.join("prompt.md")).map_err(failure)?)),
        }),
    )
}

pub fn needs_fresh(snapshot: &Snapshot, previous_run_dir: Option<&str>) -> bool {
    if !snapshot.fresh_conversations {
        return false;
    }
    let previous = previous_run_dir
        .and_then(|p| fs::read(Path::new(p).join("instructions.json")).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    previous.as_ref().and_then(|v| v["version"].as_str()) != Some(snapshot.version.as_str())
}
