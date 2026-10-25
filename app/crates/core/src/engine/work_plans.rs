//! Bounded decomposition with explicit, non-overlapping repository ownership.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPlan {
    pub summary: String,
    pub subtasks: Vec<WorkItemPlan>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkItemPlan {
    pub key: String,
    pub title: String,
    pub brief: String,
    #[serde(alias = "owned_paths")]
    pub owned_paths: Vec<String>,
    #[serde(alias = "acceptance_criteria")]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

pub fn owns_path(owner: &str, path: &str) -> bool {
    let owner = owner.trim_end_matches('/').to_lowercase();
    let path = path.trim_end_matches('/').to_lowercase();
    path == owner || path.starts_with(&(owner + "/"))
}

fn length(text: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&text.chars().count())
}

impl WorkPlan {
    pub fn validated(mut self) -> Result<Self, String> {
        if !length(&self.summary, 1, 4000) || !(1..=30).contains(&self.subtasks.len()) {
            return Err("A plan needs a summary and 1 to 30 subtasks".into());
        }
        let keys: HashSet<_> = self.subtasks.iter().map(|s| s.key.clone()).collect();
        if keys.len() != self.subtasks.len() {
            return Err("Subtask keys must be unique".into());
        }
        for item in &mut self.subtasks {
            let valid_key = item.key.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && length(&item.key, 1, 48)
                && item.key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
            if !valid_key
                || !length(&item.title, 1, 120)
                || !length(&item.brief, 1, 12000)
                || !(1..=80).contains(&item.owned_paths.len())
                || !(1..=40).contains(&item.acceptance_criteria.len())
                || item.dependencies.len() > 30
            {
                return Err("Invalid subtask fields or bounds".into());
            }
            for path in &mut item.owned_paths {
                let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
                if parts.is_empty()
                    || path.starts_with('/')
                    || path.chars().any(|c| "\\:*?[]\0\n\r".contains(c))
                    || parts
                        .iter()
                        .any(|p| ["..", ".git"].contains(&p.to_lowercase().as_str()) || p.ends_with(['.', ' ']))
                {
                    return Err("ownedPaths must contain explicit repository-relative files or directories".into());
                }
                *path = parts.join("/");
            }
        }
        let mut resolved = HashSet::new();
        while resolved.len() < keys.len() {
            let ready: Vec<_> = self
                .subtasks
                .iter()
                .filter(|s| !resolved.contains(&s.key) && s.dependencies.iter().all(|d| resolved.contains(d)))
                .map(|s| s.key.clone())
                .collect();
            if ready.is_empty() {
                return Err("Dependencies contain a cycle or unknown subtask".into());
            }
            resolved.extend(ready);
        }
        for (index, left) in self.subtasks.iter().enumerate() {
            for right in &self.subtasks[index + 1..] {
                if left.owned_paths.iter().any(|a| right.owned_paths.iter().any(|b| owns_path(a, b) || owns_path(b, a)))
                {
                    return Err(format!("Overlapping ownership: {} and {}", left.key, right.key));
                }
            }
        }
        Ok(self)
    }
}
