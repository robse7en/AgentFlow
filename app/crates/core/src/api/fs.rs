//! Directory browsing for the UI's directory pickers, ported from `backend/agentflow/api/config.py`.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::Query;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult};
use crate::paths::{py_str, resolve};

#[derive(Deserialize)]
pub struct ListQuery {
    path: Option<String>,
    /// Constrain browsing to this directory (the project picker).
    root: Option<String>,
}

fn roots() -> Vec<String> {
    if cfg!(windows) {
        ('A'..='Z').map(|letter| format!("{letter}:\\")).filter(|root| Path::new(root).exists()).collect()
    } else {
        vec!["/".into()]
    }
}

fn expand(value: &str) -> PathBuf {
    PathBuf::from(crate::process::expand_user(value))
}

/// Sub-directories, without hidden ones and node_modules, sorted case-insensitively.
fn children(target: &Path) -> Result<Vec<Value>, ApiError> {
    let entries = std::fs::read_dir(target).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => ApiError::new(403, format!("Permission denied: {}", py_str(target))),
        _ => ApiError::new(400, format!("Cannot list {}: {e}", py_str(target))),
    })?;
    let mut names: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
        .filter(|(name, path)| path.is_dir() && !name.starts_with('.') && name.to_lowercase() != "node_modules")
        .collect();
    names.sort_by_key(|(name, _)| name.to_lowercase());
    Ok(names.into_iter().map(|(name, path)| json!({"name": name, "path": py_str(&path)})).collect())
}

/// List the sub-directories of `path`. With `root`, the listing never leaves that directory.
pub async fn list_directory(Query(query): Query<ListQuery>) -> ApiResult {
    let path = query.path.filter(|p| !p.is_empty());
    if let Some(root) = query.root.filter(|r| !r.is_empty()) {
        let root_path = resolve(&expand(&root));
        if !root_path.is_dir() {
            return Err(ApiError::new(400, format!("Root does not exist: {root}")));
        }
        let mut target = match &path {
            Some(path) => resolve(&root_path.join(path)),
            None => root_path.clone(),
        };
        if !crate::paths::is_within(&target, &root_path) || !target.is_dir() {
            target = root_path.clone();
        }
        let at_root = crate::paths::same_path(&target, &root_path);
        let relative = if at_root {
            String::new()
        } else {
            crate::harness::as_posix(target.strip_prefix(&root_path).unwrap_or(&target))
        };
        return Ok(Json(json!({
            "path": py_str(&target), "relative": relative, "root": py_str(&root_path),
            "parent": if at_root { None } else { target.parent().map(py_str) },
            "entries": children(&target)?,
        })));
    }
    let home = std::env::home_dir().map(|h| py_str(&h)).unwrap_or_default();
    let Some(path) = path else {
        let roots = roots();
        let entries: Vec<Value> = roots.iter().map(|r| json!({"name": r, "path": r})).collect();
        return Ok(Json(json!({"path": null, "parent": null, "roots": roots, "home": home, "entries": entries})));
    };
    let target = expand(&path);
    if !target.is_dir() {
        return Err(ApiError::new(400, format!("Directory does not exist: {path}")));
    }
    let target = resolve(&target);
    let parent = target.parent().map(py_str);
    Ok(Json(json!({
        "path": py_str(&target), "parent": parent, "roots": roots(), "home": home,
        "entries": children(&target)?, "isGitRepo": target.join(".git").exists(),
    })))
}
