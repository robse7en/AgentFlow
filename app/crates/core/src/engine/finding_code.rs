//! The code a review finding points at, for the Findings tab (`workflow/finding_code.py`): the
//! file at the reviewed commit, the task's change to it, and the same place in the task's current
//! commit. Everything is read from the repository's object store, so it still works after the
//! task worktree is cleaned up.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::{Engine, WorkflowError};
use crate::db::{ReviewFindingRow, TaskRow, table};

/// Larger files (or diffs) are not sent to the UI.
pub const MAX_BYTES: usize = 1_000_000;

/// A finding's file as a path inside the repository (`/` separated, no leading `./`), or None
/// when it is absolute or climbs out with `..`.
pub fn repository_path(file: &str) -> Option<String> {
    let path = file.trim().replace('\\', "/");
    let path = path.trim_start_matches("./");
    let absolute = path.starts_with('/') || path.as_bytes().get(1) == Some(&b':');
    if path.is_empty() || absolute || path.split('/').any(|part| part == "..") {
        return None;
    }
    Some(path.to_string())
}

/// A file's lines without their line endings.
pub fn split_lines(text: &str) -> Vec<String> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line).to_string()).collect()
}

/// `-a,b` or `+c,d` from a hunk header: the start and count (count 1 when left out).
fn range(text: &str) -> Option<(i64, i64)> {
    let rest = text.get(1..)?;
    let (start, count) = rest.split_once(',').unwrap_or((rest, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

/// The hunks of a unified diff of one file, each with its rows numbered on both sides.
pub fn parse_hunks(diff: &str) -> Vec<Value> {
    let mut hunks: Vec<Value> = Vec::new();
    let (mut old, mut new) = (0, 0);
    for line in diff.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("@@ ") {
            let mut parts = rest.split(' ');
            let (Some((old_start, old_count)), Some((new_start, new_count))) =
                (parts.next().and_then(range), parts.next().and_then(range))
            else {
                continue;
            };
            (old, new) = (old_start, new_start);
            hunks.push(json!({"header": line, "oldStart": old_start, "oldCount": old_count,
                "newStart": new_start, "newCount": new_count, "rows": []}));
            continue;
        }
        let Some(hunk) = hunks.last_mut() else { continue };
        let (kind, text) = match line.chars().next() {
            Some(' ') => ("context", &line[1..]),
            Some('-') => ("del", &line[1..]),
            Some('+') => ("add", &line[1..]),
            // "\ No newline at end of file", or the blank line after the last row.
            _ => continue,
        };
        let row = match kind {
            "context" => json!({"kind": kind, "old": old, "new": new, "text": text}),
            "del" => json!({"kind": kind, "old": old, "new": null, "text": text}),
            _ => json!({"kind": kind, "old": null, "new": new, "text": text}),
        };
        old += i64::from(kind != "add");
        new += i64::from(kind != "del");
        hunk["rows"].as_array_mut().unwrap().push(row);
    }
    hunks
}

/// A hunk side's lines as a half-open range; git gives an empty side the line before it.
fn span(start: i64, count: i64) -> (i64, i64) {
    let start = if count == 0 { start + 1 } else { start };
    (start, start + count)
}

fn int(value: &Value, key: &str) -> i64 {
    value[key].as_i64().unwrap_or(0)
}

/// Where old line `n` is after the change the hunks describe. A deleted line maps to the next
/// line that replaced it.
pub fn map_line(hunks: &[Value], n: i64) -> i64 {
    let mut shift = 0;
    for hunk in hunks {
        let (old_start, old_end) = span(int(hunk, "oldStart"), int(hunk, "oldCount"));
        let (new_start, new_end) = span(int(hunk, "newStart"), int(hunk, "newCount"));
        if n < old_start {
            break;
        }
        if n >= old_end {
            shift = new_end - old_end;
            continue;
        }
        let rows = hunk["rows"].as_array().map(Vec::as_slice).unwrap_or_default();
        let Some(at) = rows.iter().position(|row| row["old"].as_i64() == Some(n)) else { break };
        return rows[at..]
            .iter()
            .find_map(|row| row["new"].as_i64())
            .unwrap_or_else(|| (new_end - 1).max(new_start).max(1));
    }
    n + shift
}

/// Whether the hunks delete or replace any old line from `first` to `last`.
pub fn touches(hunks: &[Value], first: i64, last: i64) -> bool {
    hunks
        .iter()
        .flat_map(|hunk| hunk["rows"].as_array().into_iter().flatten())
        .any(|row| row["kind"] == "del" && row["old"].as_i64().is_some_and(|old| (first..=last).contains(&old)))
}

/// Why a file's text can't be shown, if it can't.
fn unshowable(path: &str, text: &str) -> Option<String> {
    if text.contains('\0') {
        Some(format!("{path} is a binary file"))
    } else if text.len() > MAX_BYTES {
        Some(format!("{path} is too large to show"))
    } else {
        None
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

impl Engine {
    /// `GET /api/tasks/{key}/findings/{id}/code`.
    pub async fn finding_code(&self, jira_key: &str, finding_id: i64) -> Result<Value, WorkflowError> {
        let (task, finding) = self.db.session(|tx| {
            let task = table::select_one::<TaskRow>(tx, "WHERE jira_key = ?1", [jira_key.to_uppercase()])?
                .ok_or_else(|| WorkflowError::not_found(format!("Task {jira_key} not found")))?;
            let finding =
                table::select_one::<ReviewFindingRow>(tx, "WHERE id = ?1 AND task_id = ?2", (finding_id, task.id))?
                    .ok_or_else(|| WorkflowError::not_found(format!("Finding {finding_id} not found")))?;
            Ok::<_, WorkflowError>((task, finding))
        })?;
        let mut out = json!({"file": finding.file, "line": finding.line, "endLine": finding.end_line,
            "reviewedSha": finding.reviewed_sha, "snapshot": null, "diff": null, "now": null});
        let mut unavailable = Map::new();
        let views = self.finding_views(&task, &finding, &mut out, &mut unavailable).await;
        if let Err(message) = views {
            for view in ["snapshot", "diff", "now"] {
                if out[view].is_null() && !unavailable.contains_key(view) {
                    unavailable.insert(view.into(), json!(message));
                }
            }
        }
        out["unavailable"] = Value::Object(unavailable);
        Ok(out)
    }

    async fn finding_views(
        &self,
        task: &TaskRow,
        finding: &ReviewFindingRow,
        out: &mut Value,
        unavailable: &mut Map<String, Value>,
    ) -> Result<(), String> {
        let Some(file) = finding.file.as_deref().filter(|f| !f.trim().is_empty()) else {
            return Err("The finding names no file".into());
        };
        let Some(reviewed) = finding.reviewed_sha.as_deref().filter(|s| !s.is_empty()) else {
            return Err("The finding has no reviewed commit".into());
        };
        let Some(path) = repository_path(file) else {
            return Err(format!("{file} is outside the repository"));
        };
        let root = Path::new(&task.repository_root);
        let git = |e: crate::git::GitError| e.to_string();
        let Some(text) = self.git.file_at(root, reviewed, &path).await.map_err(git)? else {
            return Err(format!("{path} is not in {}", short(reviewed)));
        };
        if let Some(reason) = unshowable(&path, &text) {
            return Err(reason);
        }
        out["snapshot"] = json!({"sha": reviewed, "lines": split_lines(&text)});

        match self.git.fork_point(root, reviewed, &task.base_branch).await.map_err(git)? {
            None => {
                unavailable.insert("diff".into(), json!(format!("Cannot find {} to compare with", task.base_branch)));
            }
            Some(base) if base == reviewed => {
                let reason = format!("The task branch is already part of {}", task.base_branch);
                unavailable.insert("diff".into(), json!(reason));
            }
            Some(base) => {
                let diff = self.git.file_diff(root, &base, reviewed, &path).await.map_err(git)?;
                if diff.len() > MAX_BYTES {
                    unavailable.insert("diff".into(), json!(format!("The change to {path} is too large to show")));
                } else {
                    out["diff"] = json!({"from": base, "to": reviewed, "hunks": parse_hunks(&diff)});
                }
            }
        }

        let current = task.current_sha.as_deref().filter(|s| !s.is_empty()).unwrap_or(reviewed);
        let (lines, hunks) = if current == reviewed {
            (out["snapshot"]["lines"].clone(), Vec::new())
        } else {
            let Some(text) = self.git.file_at(root, current, &path).await.map_err(git)? else {
                unavailable.insert("now".into(), json!(format!("{path} is not in {}", short(current))));
                return Ok(());
            };
            if let Some(reason) = unshowable(&path, &text) {
                unavailable.insert("now".into(), json!(reason));
                return Ok(());
            }
            let diff = self.git.file_diff(root, reviewed, current, &path).await.map_err(git)?;
            (json!(split_lines(&text)), parse_hunks(&diff))
        };
        let line = finding.line.map(|n| map_line(&hunks, n));
        let end_line = finding.end_line.map(|n| map_line(&hunks, n));
        let changed = finding.line.is_some_and(|n| touches(&hunks, n, finding.end_line.unwrap_or(n)));
        out["now"] = json!({"sha": current, "lines": lines, "line": line, "endLine": end_line, "changed": changed,
            "hunks": hunks});
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n\
@@ -2,4 +2,5 @@ fn main\n a\n-b\n+B1\n+B2\n c\n d\n\
@@ -20,0 +22,2 @@\n+x\n+y\n\\ No newline at end of file\n";

    #[test]
    fn paths_stay_inside_the_repository() {
        assert_eq!(repository_path(" ./src\\a.rs ").as_deref(), Some("src/a.rs"));
        for bad in ["", "/etc/passwd", "C:/x", "a/../../b", ".."] {
            assert_eq!(repository_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn lines_drop_their_endings() {
        assert_eq!(split_lines("a\r\nb\n"), ["a", "b"]);
        assert_eq!(split_lines("a\n\nb"), ["a", "", "b"]);
        assert!(split_lines("").is_empty());
    }

    #[test]
    fn hunks_number_both_sides() {
        let hunks = parse_hunks(DIFF);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0]["header"], "@@ -2,4 +2,5 @@ fn main");
        let rows: Vec<_> = hunks[0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["kind"].as_str().unwrap(), r["old"].as_i64(), r["new"].as_i64()))
            .collect();
        assert_eq!(
            rows,
            [
                ("context", Some(2), Some(2)),
                ("del", Some(3), None),
                ("add", None, Some(3)),
                ("add", None, Some(4)),
                ("context", Some(4), Some(5)),
                ("context", Some(5), Some(6)),
            ]
        );
        assert_eq!(hunks[1]["rows"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn lines_map_through_the_change() {
        let hunks = parse_hunks(DIFF);
        assert_eq!(map_line(&hunks, 1), 1);
        assert_eq!(map_line(&hunks, 2), 2);
        // The deleted line maps to its replacement.
        assert_eq!(map_line(&hunks, 3), 3);
        assert_eq!(map_line(&hunks, 4), 5);
        assert_eq!(map_line(&hunks, 10), 11);
        assert_eq!(map_line(&hunks, 20), 21);
        // Below the insertion after old line 20.
        assert_eq!(map_line(&hunks, 21), 24);
        assert!(touches(&hunks, 3, 3) && touches(&hunks, 1, 5) && !touches(&hunks, 4, 20));
    }
}
