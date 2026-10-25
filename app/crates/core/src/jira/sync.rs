//! Incremental, bounded Jira attachment manifests and local context artifacts.
use super::{JiraError, JiraService, render};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct SkippedAttachment {
    pub id: String,
    pub filename: String,
    pub reason: String,
}
#[derive(Default, Clone, Debug)]
pub struct TicketSync {
    pub added: Vec<String>,
    pub kept: Vec<String>,
    pub removed: Vec<String>,
    pub skipped: Vec<SkippedAttachment>,
    pub frames: BTreeMap<String, usize>,
    pub frame_errors: BTreeMap<String, String>,
    pub comments_error: Option<String>,
    pub title: Option<String>,
}
impl TicketSync {
    pub fn changed(&self) -> bool {
        !self.added.is_empty()
            || !self.removed.is_empty()
            || !self.skipped.is_empty()
            || !self.frames.is_empty()
            || !self.frame_errors.is_empty()
            || self.comments_error.is_some()
    }
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("+{} new", self.added.len()), format!("{} unchanged", self.kept.len())];
        if !self.removed.is_empty() {
            parts.push(format!("{} removed from the ticket", self.removed.len()));
        }
        if !self.skipped.is_empty() {
            parts.push(format!(
                "{} skipped ({})",
                self.skipped.len(),
                self.skipped
                    .iter()
                    .map(|s| format!("{} {}: {}", s.id, s.filename, s.reason))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if !self.frames.is_empty() {
            parts.push(format!("keyframes from {} video(s)", self.frames.len()));
        }
        if !self.frame_errors.is_empty() {
            parts.push(format!("{} video(s) without keyframes", self.frame_errors.len()));
        }
        if let Some(error) = &self.comments_error {
            parts.push(format!("comments not fetched ({error})"));
        }
        parts.join(", ")
    }
}
pub(crate) fn atomic(path: &Path, text: &str) -> Result<(), JiraError> {
    let tmp =
        path.with_file_name(format!("{}.{}.tmp", path.file_name().unwrap().to_string_lossy(), crate::db::new_uid()));
    crate::pyfile::write_text(&tmp, text)?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(error.into());
    }
    Ok(())
}
fn valid_id(id: &str) -> bool {
    (1..=30).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}
fn text(value: &Value) -> String {
    value.as_str().map(String::from).unwrap_or_else(|| value.to_string())
}
fn size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
impl JiraService {
    pub async fn sync_ticket(&self, key: &str, context: &Path, creation: bool) -> Result<TicketSync, JiraError> {
        if crate::paths::linked(context).is_some() {
            return Err(JiraError("Unsafe Jira context directory".into()));
        }
        let issue = self.fetch_issue(key).await?;
        let fields = &issue["fields"];
        let mut result = TicketSync {
            title: fields["summary"].as_str().filter(|s| !s.is_empty()).map(String::from),
            ..Default::default()
        };
        let comments = if self.config.fetch_comments {
            match self.fetch_comments(key).await {
                Ok(comments) => comments,
                Err(error) => {
                    result.comments_error = Some(error.to_string());
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let attachments = context.join("attachments");
        let removed = context.join("attachments-removed");
        if crate::paths::linked(&attachments).is_some() || crate::paths::linked(&removed).is_some() {
            return Err(JiraError("Unsafe Jira attachment directory".into()));
        }
        std::fs::create_dir_all(&attachments)?;
        let manifest_path = context.join("jira-manifest.json");
        let previous = super::read_manifest(&manifest_path);
        let mut entries = previous
            .get("attachments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| Some((e["id"].as_str()?.to_string(), e.clone())))
            .filter(|(id, _)| valid_id(id))
            .collect::<BTreeMap<_, _>>();
        let mut local = BTreeMap::<String, PathBuf>::new();
        for entry in std::fs::read_dir(&attachments)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".part") {
                let _ = std::fs::remove_file(path);
                continue;
            }
            if crate::paths::linked(&path).is_some() {
                continue;
            }
            if let Some((id, _)) = name.split_once('-')
                && valid_id(id)
                && path.is_file()
            {
                local.insert(id.into(), path);
            }
        }
        let generation = previous.get("syncGeneration").and_then(Value::as_i64).unwrap_or(0).saturating_add(1);
        let legacy = previous.is_empty() && !local.is_empty();
        let last_prompted =
            if legacy { json!(0) } else { previous.get("lastPromptedGeneration").cloned().unwrap_or(Value::Null) };
        let ticket = fields["attachment"].as_array().cloned().unwrap_or_default();
        let names = ticket
            .iter()
            .filter_map(|a| Some((a["filename"].as_str()?.into(), text(&a["id"]))))
            .collect::<BTreeMap<_, _>>();
        let mut references = BTreeMap::<String, Vec<String>>::new();
        for (where_, html) in
            std::iter::once(("description".to_string(), issue["renderedFields"]["description"].as_str().unwrap_or("")))
                .chain(
                    comments
                        .iter()
                        .map(|c| (format!("comment:{}", text(&c["id"])), c["renderedBody"].as_str().unwrap_or(""))),
                )
        {
            for id in render::referenced_ids(html, &names) {
                references.entry(id).or_default().push(where_.clone());
            }
        }
        let ticket_ids = ticket.iter().map(|a| text(&a["id"])).collect::<HashSet<_>>();
        let mut total = local.iter().filter(|(id, _)| ticket_ids.contains(*id)).map(|(_, p)| size(p)).sum::<u64>();
        let mut on_ticket = HashSet::new();
        for (index, attachment) in ticket.iter().enumerate() {
            let id = text(&attachment["id"]);
            let filename = attachment["filename"].as_str().filter(|s| !s.is_empty()).unwrap_or("attachment");
            let name = render::sanitize_filename(filename);
            if !valid_id(&id) {
                result.skipped.push(SkippedAttachment {
                    id: "?".into(),
                    filename: name,
                    reason: "invalid attachment ID".into(),
                });
                continue;
            }
            on_ticket.insert(id.clone());
            let count = attachment["size"].as_u64();
            let seen = if legacy && local.contains_key(&id) { 0 } else { generation };
            let entry = entries.entry(id.clone()).or_insert_with(|| json!({"id":id,"firstSeenGeneration":seen}));
            for (key, value) in [
                ("file", json!(format!("{id}-{name}"))),
                ("filename", json!(filename)),
                ("mimeType", attachment["mimeType"].clone()),
                ("size", json!(count)),
                ("referencedIn", json!(references.get(&id).cloned().unwrap_or_default())),
                ("reason", Value::Null),
            ] {
                entry[key] = value;
            }
            if let Some(existing) = local.get(&id) {
                if count.is_none_or(|n| size(existing) == n) {
                    entry["status"] = json!("present");
                    entry["file"] = json!(existing.file_name().unwrap().to_string_lossy());
                    result.kept.push(existing.file_name().unwrap().to_string_lossy().into());
                    continue;
                }
                total = total.saturating_sub(size(existing));
                let _ = std::fs::remove_file(existing);
            }
            let dest = attachments.join(entry["file"].as_str().unwrap());
            let mut reason = self.skip_reason(index, count, total, attachment["content"].as_str());
            if reason.is_none() && crate::paths::linked(&dest).is_some() {
                reason = Some("unsafe file name".into());
            }
            if reason.is_none() {
                let part = dest.with_file_name(format!("{}.part", dest.file_name().unwrap().to_string_lossy()));
                let limit = (self.config.max_attachment_bytes as u64)
                    .min((self.config.max_total_attachment_bytes as u64).saturating_sub(total));
                match self.download(attachment["content"].as_str().unwrap(), &part, limit).await {
                    Ok(_) => {
                        if std::fs::rename(&part, &dest).is_err() {
                            reason = Some("download failed: cannot publish attachment".into());
                        }
                    }
                    Err(error) => {
                        let _ = std::fs::remove_file(&part);
                        if !dest.is_file() {
                            reason = Some(format!("download failed: {error}"));
                        }
                    }
                }
            }
            if let Some(reason) = reason {
                entry["status"] = json!("skipped");
                entry["reason"] = json!(reason);
                result.skipped.push(SkippedAttachment { id, filename: name, reason });
                continue;
            }
            total = total.saturating_add(size(&dest));
            entry["status"] = json!("present");
            result.added.push(dest.file_name().unwrap().to_string_lossy().into());
        }
        for (id, path) in local {
            if on_ticket.contains(&id) {
                continue;
            }
            std::fs::create_dir_all(&removed)?;
            if std::fs::rename(&path, removed.join(path.file_name().unwrap())).is_err() {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            result.removed.push(name.clone());
            entries.entry(id.clone()).or_insert_with(
                || json!({"id":id,"file":name,"filename":name,"firstSeenGeneration":if legacy {0}else{generation}}),
            );
        }
        for (id, entry) in &mut entries {
            if !on_ticket.contains(id) && entry["status"] != "removed" {
                entry["status"] = json!("removed");
                entry["removedGeneration"] = json!(generation);
                entry["referencedIn"] = json!([]);
                entry["reason"] = Value::Null;
            }
        }
        if !creation && self.config.max_frames_per_video > 0 {
            self.sync_frames(context, &mut entries, &mut result).await;
        }
        let markdown = render::issue_markdown(
            key,
            fields,
            &issue["renderedFields"],
            self.config.fetch_comments.then_some(comments.as_slice()),
            &entries,
            result.comments_error.as_deref(),
        );
        let mut entries = entries.into_values().collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            let (a, b) = (a["id"].as_str().unwrap(), b["id"].as_str().unwrap());
            a.len().cmp(&b.len()).then(a.cmp(b))
        });
        let manifest = json!({"issueKey":key,"syncedAt":chrono::Utc::now().to_rfc3339(),"ticketUpdated":fields["updated"],"syncGeneration":generation,"lastPromptedGeneration":last_prompted,"commentsError":result.comments_error,"attachments":entries});
        atomic(&context.join("jira-issue.md"), &markdown)?;
        atomic(&manifest_path, &(pyjson::dumps(&manifest) + "\n"))?;
        Ok(result)
    }
    fn skip_reason(&self, index: usize, size: Option<u64>, total: u64, url: Option<&str>) -> Option<String> {
        let cfg = &self.config;
        if url.is_none_or(|s| s.is_empty()) {
            return Some("Jira gave no download URL".into());
        }
        if index >= cfg.max_attachments as usize {
            return Some(format!("over the maxAttachments limit ({})", cfg.max_attachments));
        }
        if size.is_some_and(|s| s > cfg.max_attachment_bytes as u64) {
            return Some(format!(
                "{:.1} MB is over maxAttachmentBytes ({:.1} MB)",
                size.unwrap() as f64 / 1_000_000.0,
                cfg.max_attachment_bytes as f64 / 1_000_000.0
            ));
        }
        if size.is_some_and(|s| total.saturating_add(s) > cfg.max_total_attachment_bytes as u64) {
            return Some(format!(
                "would exceed maxTotalAttachmentBytes ({:.1} MB)",
                cfg.max_total_attachment_bytes as f64 / 1_000_000.0
            ));
        }
        None
    }
    async fn sync_frames(&self, context: &Path, entries: &mut BTreeMap<String, Value>, result: &mut TicketSync) {
        let budget = std::time::Duration::from_secs_f64(self.config.frame_extraction_timeout_seconds.get());
        let deadline = std::time::Instant::now() + budget;
        for (id, entry) in entries {
            let Some(file) =
                entry["file"].as_str().filter(|s| !s.contains(['/', '\\', ':']) && *s != "." && *s != "..")
            else {
                continue;
            };
            if entry["status"] != "present" || !crate::media::is_video(file, entry["mimeType"].as_str()) {
                continue;
            }
            let source = context.join("attachments").join(file);
            let dest = context.join("frames").join(id);
            if crate::paths::linked(&source).is_some() || crate::paths::linked(&dest).is_some() {
                entry["framesError"] = json!("Unsafe video path");
                continue;
            }
            let done = crate::media::frames_done(&dest);
            if !done.is_empty() || entry["framesChecked"] == true {
                entry["frames"] = if done.is_empty() { Value::Null } else { json!(done) };
                continue;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.as_secs_f64() < 1.0 {
                let reason = "deferred to the next sync (frame-extraction time budget used up)";
                entry["frames"] = Value::Null;
                entry["framesError"] = json!(reason);
                result.frame_errors.insert(id.clone(), reason.into());
                continue;
            }
            match crate::media::extract_frames(
                &source,
                &dest,
                self.config.max_frames_per_video as usize,
                remaining,
                self.config.ffmpeg_path.as_deref(),
            )
            .await
            {
                Ok(names) => {
                    entry["frames"] = if names.is_empty() { Value::Null } else { json!(names) };
                    entry["framesChecked"] = json!(true);
                    entry["framesError"] = Value::Null;
                    if !names.is_empty() {
                        result.frames.insert(id.clone(), names.len());
                    }
                }
                Err(error) => {
                    let cut_short =
                        error.message.contains("timed out") && remaining.as_secs_f64() < budget.as_secs_f64() - 1.0;
                    entry["frames"] = Value::Null;
                    entry["framesError"] = json!(error.message);
                    entry["framesChecked"] = json!(!(error.tool_missing || cut_short));
                    result.frame_errors.insert(id.clone(), error.message);
                }
            }
        }
    }
}
