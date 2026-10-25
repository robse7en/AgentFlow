//! Projects: named groups of tasks with defaults for the New Task form, ported from the project
//! actions of `backend/agentflow/workflow/engine.py` and `workflow/projects.py`.

use std::collections::HashSet;
use std::path::Path;

use indexmap::IndexMap;
use rusqlite::Transaction;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use super::lifecycle::{AgentOverride, git_error, working_dir};
use super::snapshot::iso;
use super::store::TaskState;
use super::{Engine, WorkflowError};
use crate::db::table::{self, Tracked};
use crate::db::{DbTime, Json, ProjectRow, TaskRow, WorkflowEventRow};

/// Names, not colours: the UI maps each to a light and a dark value.
pub const PROJECT_COLORS: [&str; 8] = ["blue", "purple", "orange", "teal", "green", "red", "pink", "slate"];
/// `?project=none` is the dashboard's "No project" filter.
const RESERVED_SLUGS: [&str; 1] = ["none"];
const MAX_SLUG_LENGTH: usize = 48;

/// Lowercase ASCII words joined by '-'; "project" when the name has none.
pub fn slug_base(name: &str) -> String {
    let lower = name.to_lowercase();
    let words: Vec<&str> =
        lower.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit())).filter(|w| !w.is_empty()).collect();
    let joined = words.join("-");
    let slug = joined[..joined.len().min(MAX_SLUG_LENGTH)].trim_matches('-');
    if slug.is_empty() { "project".into() } else { slug.into() }
}

pub fn unique_slug(name: &str, taken: &HashSet<&str>) -> String {
    let base = slug_base(name);
    let mut slug = base.clone();
    let mut n = 1;
    while taken.contains(slug.as_str()) || RESERVED_SLUGS.contains(&slug.as_str()) {
        n += 1;
        let suffix = format!("-{n}");
        slug = format!("{}{suffix}", &base[..base.len().min(MAX_SLUG_LENGTH - suffix.len())]);
    }
    slug
}

/// The least-used colour, in palette order, so a new project stands apart from the rest.
pub fn next_color(used: &[&str]) -> &'static str {
    PROJECT_COLORS
        .iter()
        .min_by_key(|color| used.iter().filter(|u| *u == *color).count())
        .copied()
        .unwrap_or(PROJECT_COLORS[0])
}

pub fn project_dict(project: &ProjectRow) -> Value {
    let overrides = match &project.agent_overrides.0 {
        Value::Null => json!({}),
        overrides => overrides.clone(),
    };
    json!({
        "id": project.id, "slug": project.slug, "name": project.name, "color": project.color,
        "description": project.description,
        "defaultRepositoryPath": project.default_repository_path,
        "defaultProjectRelativePath": project.default_project_relative_path,
        "defaultBaseBranch": project.default_base_branch,
        "agentOverrides": overrides,
        "archivedAt": iso(project.archived_at.as_ref()),
        "createdAt": iso(Some(&project.created_at)), "updatedAt": iso(Some(&project.updated_at)),
    })
}

/// A field that was sent, null or not (the outer Option of a [`ProjectRequest`] field).
fn sent<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A field that must be sent but may be null.
pub(crate) fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

/// POST /api/projects and PATCH /api/projects/{id} (workflow/models.py ProjectRequest): an edit
/// changes only the fields it sends, and null clears one. The outer Option is whether it was sent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectRequest {
    #[serde(default, deserialize_with = "sent")]
    pub name: Option<Option<String>>,
    #[serde(default, deserialize_with = "sent")]
    pub color: Option<Option<String>>,
    #[serde(default, deserialize_with = "sent")]
    pub description: Option<Option<String>>,
    #[serde(rename = "defaultRepositoryPath", default, deserialize_with = "sent")]
    pub default_repository_path: Option<Option<String>>,
    #[serde(rename = "defaultProjectRelativePath", default, deserialize_with = "sent")]
    pub default_project_relative_path: Option<Option<String>>,
    #[serde(rename = "defaultBaseBranch", default, deserialize_with = "sent")]
    pub default_base_branch: Option<Option<String>>,
    #[serde(rename = "agentOverrides", default, deserialize_with = "sent")]
    pub agent_overrides: Option<Option<IndexMap<String, AgentOverride>>>,
}

impl ProjectRequest {
    /// Pydantic's constraints and validators: lengths, a known colour, and text fields trimmed
    /// with blank meaning none.
    pub fn validated(mut self) -> Result<Self, String> {
        let limits = [
            ("name", &self.name, 80),
            ("description", &self.description, 2000),
            ("defaultBaseBranch", &self.default_base_branch, 255),
        ];
        for (field, value, max) in limits {
            if let Some(Some(text)) = value
                && text.chars().count() > max
            {
                return Err(format!("{field}: at most {max} characters"));
            }
        }
        if let Some(Some(color)) = &self.color
            && !PROJECT_COLORS.contains(&color.as_str())
        {
            return Err(format!("color: Colour must be one of: {}", PROJECT_COLORS.join(", ")));
        }
        let sent_text = [
            &mut self.name,
            &mut self.description,
            &mut self.default_repository_path,
            &mut self.default_project_relative_path,
            &mut self.default_base_branch,
        ];
        for text in sent_text.into_iter().flatten() {
            *text = text.take().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        }
        Ok(self)
    }
}

/// Empty fields and roles dropped, as task creation stores them.
fn clean_overrides(overrides: Option<&IndexMap<String, AgentOverride>>) -> Map<String, Value> {
    overrides
        .into_iter()
        .flatten()
        .map(|(role, o)| (role.clone(), o.without_nones()))
        .filter(|(_, o)| !o.is_empty())
        .map(|(role, o)| (role, Value::Object(o)))
        .collect()
}

fn load_project(tx: &Transaction, project_id: i64) -> Result<ProjectRow, WorkflowError> {
    table::select_one::<ProjectRow>(tx, "WHERE id = ?1", [project_id])?
        .ok_or_else(|| WorkflowError::not_found(format!("Project {project_id} not found")))
}

impl Engine {
    /// Every project, archived ones included (their tasks still show them), by name.
    pub fn list_projects(&self) -> Result<Vec<Value>, WorkflowError> {
        let projects = self.db.session(|tx| table::select::<ProjectRow>(tx, "ORDER BY lower(name)", []))?;
        Ok(projects.iter().map(project_dict).collect())
    }

    fn publish_projects(&self) -> Result<(), WorkflowError> {
        let projects = self.list_projects()?;
        self.broadcaster.broadcast(&json!({"type": "projects", "projects": projects}));
        Ok(())
    }

    pub(crate) fn assignable_project(&self, tx: &Transaction, project_id: i64) -> Result<ProjectRow, WorkflowError> {
        let project = load_project(tx, project_id)?;
        if project.archived_at.is_some() {
            return Err(WorkflowError::conflict(format!(
                "Project {} is archived. Unarchive it to add tasks to it.",
                project.name
            )));
        }
        Ok(project)
    }

    /// Copy the fields `request` sent onto `project`, checking the name and the default directories.
    async fn apply_project_fields(
        &self,
        project: &mut ProjectRow,
        request: &ProjectRequest,
    ) -> Result<(), WorkflowError> {
        if request.name.is_some() || project.name.is_empty() {
            let Some(name) = request.name.clone().flatten() else {
                return Err(WorkflowError::bad_request("A project needs a name"));
            };
            let clash = self.db.session(|tx| {
                table::select_one::<ProjectRow>(
                    tx,
                    "WHERE lower(name) = ?1 AND id != ?2",
                    (name.to_lowercase(), project.id),
                )
            })?;
            if let Some(clash) = clash {
                return Err(WorkflowError::conflict(format!("A project named {} already exists", clash.name)));
            }
            project.name = name;
        }
        if let Some(Some(color)) = &request.color {
            project.color = color.clone();
        }
        if let Some(description) = &request.description {
            project.description = description.clone();
        }
        if let Some(branch) = &request.default_base_branch {
            project.default_base_branch = branch.clone();
        }
        if let Some(overrides) = &request.agent_overrides {
            project.agent_overrides = Json(Value::Object(clean_overrides(overrides.as_ref())));
        }
        if request.default_repository_path.is_some() || request.default_project_relative_path.is_some() {
            let repository =
                request.default_repository_path.clone().unwrap_or_else(|| project.default_repository_path.clone());
            let relative = request
                .default_project_relative_path
                .clone()
                .unwrap_or_else(|| project.default_project_relative_path.clone());
            match repository {
                None if relative.is_some() => {
                    return Err(WorkflowError::bad_request("A default working directory needs a default repository"));
                }
                None => {
                    project.default_repository_path = None;
                    project.default_project_relative_path = None;
                }
                Some(repository) => {
                    let info = self.git.inspect_repository(&repository).await.map_err(|e| git_error(e, 400))?;
                    project.default_project_relative_path = working_dir(Path::new(&info.root), relative.as_deref())?;
                    project.default_repository_path = Some(info.root);
                }
            }
        }
        Ok(())
    }

    pub async fn create_project(&self, request: ProjectRequest) -> Result<Value, WorkflowError> {
        let _held = self.projects_lock.lock().await;
        let existing = self.db.session(|tx| table::select::<ProjectRow>(tx, "", []))?;
        let used: Vec<&str> = existing.iter().map(|p| p.color.as_str()).collect();
        let now = DbTime::now();
        let mut project = ProjectRow {
            id: 0,
            slug: String::new(),
            name: String::new(),
            color: request.color.clone().flatten().unwrap_or_else(|| next_color(&used).into()),
            description: None,
            default_repository_path: None,
            default_project_relative_path: None,
            default_base_branch: None,
            agent_overrides: Json(json!({})),
            archived_at: None,
            created_at: now,
            updated_at: now,
        };
        self.apply_project_fields(&mut project, &request).await?;
        project.slug = unique_slug(&project.name, &existing.iter().map(|p| p.slug.as_str()).collect());
        self.db.session(|tx| table::insert(tx, &mut project))?;
        self.publish_projects()?;
        Ok(project_dict(&project))
    }

    pub async fn update_project(&self, project_id: i64, request: ProjectRequest) -> Result<Value, WorkflowError> {
        let _held = self.projects_lock.lock().await;
        let original = self.db.session(|tx| load_project(tx, project_id))?;
        let mut project = original.clone();
        self.apply_project_fields(&mut project, &request).await?;
        let project = self.db.session(|tx| {
            table::update(tx, &original, &project)?;
            load_project(tx, project_id)
        })?;
        self.publish_projects()?;
        Ok(project_dict(&project))
    }

    pub async fn set_project_archived(&self, project_id: i64, archived: bool) -> Result<Value, WorkflowError> {
        let _held = self.projects_lock.lock().await;
        let project = self.db.session(|tx| {
            let mut project = Tracked::loaded(load_project(tx, project_id)?);
            if archived != project.archived_at.is_some() {
                project.archived_at = archived.then(DbTime::now);
            }
            project.flush(tx)?;
            load_project(tx, project_id)
        })?;
        self.publish_projects()?;
        Ok(project_dict(&project))
    }

    /// Remove the project; its tasks stay, with no project.
    pub async fn delete_project(&self, project_id: i64) -> Result<Value, WorkflowError> {
        let held = self.projects_lock.lock().await;
        let keys = self.db.session(|tx| -> Result<Vec<String>, WorkflowError> {
            let project = load_project(tx, project_id)?;
            let mut keys = Vec::new();
            for mut task in table::load::<TaskRow>(tx, "WHERE project_id = ?1 ORDER BY id", [project_id])? {
                task.project_id = None;
                task.flush(tx)?;
                let message = format!("Removed from project {}, which was deleted", project.name);
                tracing::info!("[{}] {message}", task.jira_key);
                let mut event = WorkflowEventRow {
                    id: 0,
                    task_id: task.id,
                    timestamp: DbTime::now(),
                    level: "info".into(),
                    node_key: None,
                    message,
                    data: Json(Value::Null),
                };
                table::insert(tx, &mut event)?;
                keys.push(task.jira_key.clone());
            }
            table::delete(tx, &project)?;
            Ok(keys)
        })?;
        self.publish_projects()?;
        drop(held);
        for key in &keys {
            self.publish(key)?;
        }
        Ok(json!({"deleted": project_id, "unassignedTasks": keys}))
    }

    pub async fn set_task_project(&self, jira_key: &str, project_id: Option<i64>) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let projects_held = self.projects_lock.lock().await;
        self.db.session(|tx| -> Result<(), WorkflowError> {
            let mut state = TaskState::load(tx, jira_key)?;
            if state.task.project_id != project_id {
                let before = match state.task.project_id {
                    Some(id) => table::select_one::<ProjectRow>(tx, "WHERE id = ?1", [id])?,
                    None => None,
                };
                let after = project_id.map(|id| self.assignable_project(tx, id)).transpose()?;
                state.task.project_id = project_id;
                if let Some(after) = after {
                    state.info(format!("Moved to project {}", after.name));
                } else if let Some(before) = before {
                    state.info(format!("Removed from project {}", before.name));
                }
            }
            state.flush(tx)?;
            Ok(())
        })?;
        drop(projects_held);
        drop(held);
        self.publish(jira_key)
    }
}
