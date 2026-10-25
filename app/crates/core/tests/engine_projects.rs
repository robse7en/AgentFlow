//! Projects: named groups of tasks with New Task form defaults. Ported by name from
//! backend/tests/test_projects.py.

mod engine_support;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use agentflow_core::api::{self, App};
use agentflow_core::engine::lifecycle::CreateTaskRequest;
use agentflow_core::engine::projects::{PROJECT_COLORS, ProjectRequest, slug_base, unique_slug};
use agentflow_core::vcs::Outcome;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use engine_support::{Harness, engine};
use serde_json::{Value, json};
use tower::ServiceExt;

fn project(fields: Value) -> ProjectRequest {
    serde_json::from_value::<ProjectRequest>(fields).unwrap().validated().unwrap()
}

fn task_project(h: &Harness) -> Option<i64> {
    h.load().task.project_id
}

fn events(h: &Harness) -> Vec<String> {
    h.engine
        .db
        .session(|tx| {
            let mut statement = tx.prepare("SELECT message FROM workflow_events ORDER BY id")?;
            statement.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap()
}

/// Every message broadcast from now on.
fn broadcasts(h: &Harness) -> Arc<Mutex<Vec<Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    h.engine.broadcaster.subscribe(move |message| sink.lock().unwrap().push(message.clone()));
    seen
}

/// A repository root with two app directories; the git inspection is stubbed to it.
fn repo(h: &Harness) -> std::path::PathBuf {
    let root = h.tmp().join("mono");
    std::fs::create_dir_all(root.join("Apps").join("Billing")).unwrap();
    std::fs::create_dir_all(root.join("Apps").join("Accounts")).unwrap();
    h.git.returns("inspect_repository", Outcome::Ok(json!({"path": root, "root": root, "branches": ["main"]})));
    root
}

// naming ----------------------------------------------------------------------------------------

#[test]
fn test_slug_is_lowercase_ascii_words() {
    for (name, slug) in [
        ("Billing App", "billing-app".to_string()),
        ("  Accounts: Statements & PDFs!  ", "accounts-statements-pdfs".to_string()),
        ("日本", "project".to_string()),
        (&"x".repeat(100), "x".repeat(48)),
    ] {
        assert_eq!(slug_base(name), slug, "{name}");
    }
}

#[test]
fn test_slug_skips_taken_and_reserved_ones() {
    assert_eq!(unique_slug("None", &HashSet::new()), "none-2");
    assert_eq!(unique_slug("Billing", &HashSet::from(["billing", "billing-2"])), "billing-3");
    let long = "y".repeat(48);
    assert_eq!(unique_slug(&"y".repeat(60), &HashSet::from([long.as_str()])), format!("{}-2", "y".repeat(46)));
}

#[test]
fn test_an_unknown_colour_is_refused() {
    let request: ProjectRequest = serde_json::from_value(json!({"name": "Billing", "color": "magenta"})).unwrap();
    assert!(request.validated().unwrap_err().contains("Colour must be one of"));
}

// create, edit, archive, delete -----------------------------------------------------------------

#[tokio::test]
async fn test_create_picks_a_slug_and_the_least_used_colour() {
    let h = engine().await;
    let first = h.engine.create_project(project(json!({"name": "  Billing App  "}))).await.unwrap();
    let second =
        h.engine.create_project(project(json!({"name": "Admin App", "description": "Back office"}))).await.unwrap();
    assert_eq!((first["name"].as_str(), first["slug"].as_str()), (Some("Billing App"), Some("billing-app")));
    assert_eq!(first["color"], PROJECT_COLORS[0]);
    assert_eq!(second["color"], PROJECT_COLORS[1]);
    assert_eq!(second["description"], "Back office");
    assert_eq!(first["agentOverrides"], json!({}));
    assert!(first["archivedAt"].is_null() && first["createdAt"].is_string());
    let names: Vec<Value> = h.engine.list_projects().unwrap().iter().map(|p| p["name"].clone()).collect();
    assert_eq!(names, [json!("Admin App"), json!("Billing App")]);
}

#[tokio::test]
async fn test_a_project_needs_a_unique_name_ignoring_case() {
    let h = engine().await;
    let blank = h.engine.create_project(project(json!({"name": "   "}))).await.unwrap_err();
    assert!(blank.message.contains("needs a name"), "{blank}");
    let created = h.engine.create_project(project(json!({"name": "Billing"}))).await.unwrap();
    let refused = h.engine.create_project(project(json!({"name": "BILLING"}))).await.unwrap_err();
    assert!(refused.message.contains("already exists") && refused.status == 409, "{refused}");
    let id = created["id"].as_i64().unwrap();
    let renamed = h.engine.update_project(id, project(json!({"name": "billing"}))).await.unwrap();
    assert_eq!(renamed["name"], "billing");
    let cleared = h.engine.update_project(id, project(json!({"name": null}))).await.unwrap_err();
    assert!(cleared.message.contains("needs a name"), "{cleared}");
}

#[tokio::test]
async fn test_renaming_keeps_the_slug() {
    let h = engine().await;
    let created = h.engine.create_project(project(json!({"name": "Billing App"}))).await.unwrap();
    let id = created["id"].as_i64().unwrap();
    let renamed = h.engine.update_project(id, project(json!({"name": "Billing and Invoices"}))).await.unwrap();
    assert_eq!(renamed["slug"], "billing-app");
    assert_eq!(
        h.engine.create_project(project(json!({"name": "Billing App"}))).await.unwrap()["slug"],
        "billing-app-2"
    );
}

#[tokio::test]
async fn test_an_edit_changes_only_the_fields_it_sends() {
    let h = engine().await;
    let created = h
        .engine
        .create_project(project(
            json!({"name": "Billing", "description": "Invoice pages", "color": "teal", "defaultBaseBranch": "develop"}),
        ))
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();
    let unchanged = h.engine.update_project(id, project(json!({}))).await.unwrap();
    assert_eq!(
        (&unchanged["description"], &unchanged["color"], &unchanged["defaultBaseBranch"]),
        (&json!("Invoice pages"), &json!("teal"), &json!("develop"))
    );
    let cleared =
        h.engine.update_project(id, project(json!({"description": null, "defaultBaseBranch": "  "}))).await.unwrap();
    assert!(cleared["description"].is_null() && cleared["defaultBaseBranch"].is_null());
    assert_eq!(cleared["color"], "teal");
}

#[tokio::test]
async fn test_default_repository_and_working_directory_are_checked() {
    let h = engine().await;
    let root = repo(&h);
    let created = h
        .engine
        .create_project(project(json!({"name": "Billing", "defaultRepositoryPath": root,
                                       "defaultProjectRelativePath": "Apps\\Billing\\"})))
        .await
        .unwrap();
    assert_eq!(created["defaultRepositoryPath"], json!(root));
    assert_eq!(created["defaultProjectRelativePath"], "Apps/Billing");
    let id = created["id"].as_i64().unwrap();
    // Changing the directory alone is checked against the repository already stored.
    let moved = h.engine.update_project(id, project(json!({"defaultProjectRelativePath": "Apps/Accounts"}))).await;
    assert_eq!(moved.unwrap()["defaultProjectRelativePath"], "Apps/Accounts");
    let at_root = h.engine.update_project(id, project(json!({"defaultProjectRelativePath": "."}))).await.unwrap();
    assert!(at_root["defaultProjectRelativePath"].is_null());

    let missing = h.engine.update_project(id, project(json!({"defaultProjectRelativePath": "Apps/Nope"}))).await;
    assert!(missing.unwrap_err().message.contains("does not exist"));
    let outside = h.engine.update_project(id, project(json!({"defaultProjectRelativePath": ".."}))).await;
    assert!(outside.unwrap_err().message.contains("inside the repository"));
    let orphan = h
        .engine
        .create_project(project(json!({"name": "Admin", "defaultProjectRelativePath": "Apps/Billing"})))
        .await
        .unwrap_err();
    assert!(orphan.message.contains("needs a default repository"), "{orphan}");

    let cleared = h.engine.update_project(id, project(json!({"defaultRepositoryPath": null}))).await.unwrap();
    assert!(cleared["defaultRepositoryPath"].is_null() && cleared["defaultProjectRelativePath"].is_null());

    h.git.returns("inspect_repository", Outcome::Failed("Not a git repository: D:/nowhere".into()));
    let not_git = h.engine.update_project(id, project(json!({"defaultRepositoryPath": "D:/nowhere"}))).await;
    assert!(not_git.unwrap_err().message.contains("Not a git repository"));
}

#[tokio::test]
async fn test_agent_overrides_drop_empty_roles() {
    let h = engine().await;
    let created = h
        .engine
        .create_project(project(json!({"name": "Billing", "agentOverrides": {
            "implementer": {"harness": "codex", "model": null}, "code_reviewer": {}}})))
        .await
        .unwrap();
    assert_eq!(created["agentOverrides"], json!({"implementer": {"harness": "codex"}}));
    let id = created["id"].as_i64().unwrap();
    let cleared = h.engine.update_project(id, project(json!({"agentOverrides": null}))).await.unwrap();
    assert_eq!(cleared["agentOverrides"], json!({}));
}

#[tokio::test]
async fn test_project_changes_are_broadcast() {
    let h = engine().await;
    let seen = broadcasts(&h);
    let created = h.engine.create_project(project(json!({"name": "Billing"}))).await.unwrap();
    h.engine.set_project_archived(created["id"].as_i64().unwrap(), true).await.unwrap();
    let messages = seen.lock().unwrap().clone();
    let types: Vec<&Value> = messages.iter().map(|m| &m["type"]).collect();
    assert_eq!(types, [&json!("projects"), &json!("projects")]);
    assert!(messages[1]["projects"][0]["archivedAt"].is_string());
}

#[tokio::test]
async fn test_unknown_project_is_404() {
    let h = engine().await;
    let errors = [
        h.engine.update_project(9, project(json!({"name": "x"}))).await.unwrap_err(),
        h.engine.set_project_archived(9, true).await.unwrap_err(),
        h.engine.delete_project(9).await.unwrap_err(),
        h.engine.set_task_project("TEST-1", Some(9)).await.unwrap_err(),
    ];
    for error in errors {
        assert!(error.message.contains("Project 9 not found") && error.status == 404, "{error}");
    }
}

// assigning tasks -------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_task_moves_between_projects_and_out() {
    let h = engine().await;
    let billing = h.engine.create_project(project(json!({"name": "Billing"}))).await.unwrap()["id"].as_i64().unwrap();
    let admin = h.engine.create_project(project(json!({"name": "Admin"}))).await.unwrap()["id"].as_i64().unwrap();
    let snapshot = h.engine.set_task_project("TEST-1", Some(billing)).await.unwrap();
    assert_eq!(snapshot["projectId"], billing);
    let listed: Vec<Value> =
        h.engine.list_tasks("exclude").await.unwrap().iter().map(|t| t["projectId"].clone()).collect();
    assert_eq!(listed, [json!(billing)]);
    h.engine.set_task_project("TEST-1", Some(admin)).await.unwrap();
    assert!(h.engine.set_task_project("TEST-1", None).await.unwrap()["projectId"].is_null());
    let moves: Vec<String> = events(&h).into_iter().filter(|m| m.contains("project")).collect();
    assert_eq!(moves, ["Moved to project Billing", "Moved to project Admin", "Removed from project Admin"]);
}

#[tokio::test]
async fn test_an_archived_project_takes_no_new_tasks_but_keeps_its_own() {
    let h = engine().await;
    let billing = h.engine.create_project(project(json!({"name": "Billing"}))).await.unwrap()["id"].as_i64().unwrap();
    h.engine.set_task_project("TEST-1", Some(billing)).await.unwrap();
    assert!(h.engine.set_project_archived(billing, true).await.unwrap()["archivedAt"].is_string());
    // Already there: nothing to refuse.
    assert_eq!(h.engine.set_task_project("TEST-1", Some(billing)).await.unwrap()["projectId"], billing);
    h.engine.set_task_project("TEST-1", None).await.unwrap();
    let refused = h.engine.set_task_project("TEST-1", Some(billing)).await.unwrap_err();
    assert!(refused.message.contains("archived") && refused.status == 409, "{refused}");
    assert!(h.engine.set_project_archived(billing, false).await.unwrap()["archivedAt"].is_null());
    assert_eq!(h.engine.set_task_project("TEST-1", Some(billing)).await.unwrap()["projectId"], billing);
}

#[tokio::test]
async fn test_deleting_a_project_keeps_its_tasks_and_publishes_them() {
    let h = engine().await;
    let billing = h.engine.create_project(project(json!({"name": "Billing"}))).await.unwrap()["id"].as_i64().unwrap();
    h.engine.set_task_project("TEST-1", Some(billing)).await.unwrap();
    let seen = broadcasts(&h);
    let result = h.engine.delete_project(billing).await.unwrap();
    assert_eq!(result, json!({"deleted": billing, "unassignedTasks": ["TEST-1"]}));
    assert_eq!(task_project(&h), None);
    assert!(h.engine.list_projects().unwrap().is_empty());
    let messages = seen.lock().unwrap().clone();
    assert_eq!(messages[0], json!({"type": "projects", "projects": []}));
    assert_eq!(messages[1]["type"], "task");
    assert!(messages[1]["task"]["projectId"].is_null());
    assert_eq!(events(&h).last().unwrap(), "Removed from project Billing, which was deleted");
}

// creating a task in a project ------------------------------------------------------------------

fn creating(h: &Harness, project_id: i64) -> CreateTaskRequest {
    let root = repo(h);
    h.git.returns("create_worktree", Outcome::Ok(json!({"branch": "agent/LOCAL"})));
    serde_json::from_value::<CreateTaskRequest>(json!({"taskSource": "local", "prompt": "Fix the billing page",
        "repositoryPath": root, "baseBranch": "main", "projectId": project_id}))
    .unwrap()
    .validated()
    .unwrap()
}

#[tokio::test]
async fn test_a_new_task_can_start_in_a_project() {
    let h = engine().await;
    let billing = h.engine.create_project(project(json!({"name": "Billing"}))).await.unwrap()["id"].as_i64().unwrap();
    let snapshot = h.engine.create_task(creating(&h, billing)).await.unwrap();
    assert_eq!(snapshot["projectId"], billing);
}

#[tokio::test]
async fn test_a_new_task_is_refused_an_unknown_or_archived_project_before_any_worktree() {
    let h = engine().await;
    let unknown = h.engine.create_task(creating(&h, 42)).await.unwrap_err();
    assert!(unknown.message.contains("not found"), "{unknown}");
    let old = h.engine.create_project(project(json!({"name": "Old"}))).await.unwrap()["id"].as_i64().unwrap();
    h.engine.set_project_archived(old, true).await.unwrap();
    let archived = h.engine.create_task(creating(&h, old)).await.unwrap_err();
    assert!(archived.message.contains("archived"), "{archived}");
    assert!(h.git.calls_of("create_worktree").is_empty());
}

#[tokio::test]
async fn test_a_project_deleted_during_creation_leaves_the_task_without_one() {
    let h = engine().await;
    let billing = h.engine.create_project(project(json!({"name": "Billing"}))).await.unwrap()["id"].as_i64().unwrap();
    let request = creating(&h, billing);
    let db = h.engine.db.clone();
    h.git.before("create_worktree", move || {
        db.session(|tx| tx.execute("DELETE FROM projects WHERE id = ?1", [billing])).unwrap();
    });
    let snapshot = h.engine.create_task(request).await.unwrap();
    assert!(snapshot["projectId"].is_null());
    assert!(events(&h).contains(
        &"The chosen project was deleted while the task was being created, so the task has no project".to_string()
    ));
}

// HTTP ------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_projects_over_http() {
    let h = engine().await;
    let (status, created) = h.route("POST", "/api/projects", Some(json!({"name": "Billing", "color": "teal"}))).await;
    assert_eq!(status, 201);
    let id = created["id"].as_i64().unwrap();
    let bad_colour = h.route("POST", "/api/projects", Some(json!({"name": "Admin", "color": "magenta"}))).await;
    assert_eq!(bad_colour.0, 422);
    assert_eq!(h.route("POST", "/api/projects", Some(json!({"name": "billing"}))).await.0, 409);
    let (_, edited) =
        h.route("PATCH", &format!("/api/projects/{id}"), Some(json!({"description": "Invoice pages"}))).await;
    assert_eq!(edited["description"], "Invoice pages");
    let (_, moved) = h.route("PUT", "/api/tasks/TEST-1/project", Some(json!({"projectId": id}))).await;
    assert_eq!(moved["projectId"], id);
    assert_eq!(h.route("PUT", "/api/tasks/TEST-1/project", Some(json!({}))).await.0, 422);
    assert_eq!(h.route("GET", "/api/tasks", None).await.1[0]["projectId"], id);
    assert!(h.route("POST", &format!("/api/projects/{id}/archive"), None).await.1["archivedAt"].is_string());
    assert!(h.route("POST", &format!("/api/projects/{id}/unarchive"), None).await.1["archivedAt"].is_null());
    assert_eq!(h.route("GET", "/api/projects", None).await.1, json!([h.engine.list_projects().unwrap()[0]]));
    assert_eq!(h.route("GET", "/api/projects", None).await.1[0]["slug"], "billing");
    let (_, deleted) = h.route("DELETE", &format!("/api/projects/{id}"), None).await;
    assert_eq!(deleted["unassignedTasks"], json!(["TEST-1"]));
    assert_eq!(h.route("DELETE", &format!("/api/projects/{id}"), None).await.0, 404);
}

#[tokio::test]
async fn test_projects_are_human_actions() {
    let h = engine().await;
    let app = App::around(h.engine.clone(), h.tmp().join("config.json"));
    let call = |method: &str, uri: &str, agent: bool| {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1")
            .header("content-type", "application/json");
        if agent {
            request = request.header("authorization", "Bearer test-token");
        }
        request.body(Body::from(r#"{"name": "x", "projectId": null}"#)).unwrap()
    };
    for (method, uri) in [("GET", "/api/projects"), ("POST", "/api/projects"), ("PUT", "/api/tasks/TEST-1/project")] {
        let response = api::router(app.clone()).oneshot(call(method, uri, true)).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
    }
    let unpaired = api::router(app.clone()).oneshot(call("GET", "/api/projects", false)).await.unwrap();
    assert_eq!(unpaired.status(), StatusCode::UNAUTHORIZED);
}
