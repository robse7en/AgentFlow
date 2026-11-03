//! Reviewed lessons in the engine: agents propose, a human approves them into the role's repository
//! guidance in the original checkout, uncommitted. Ported by name from backend/tests/test_lessons.py.

mod engine_support;

use std::path::PathBuf;

use agentflow_core::engine::{lessons, spec};
use agentflow_core::vcs::Outcome;
use engine_support::{Harness, engine};
use serde_json::{Value, json};

async fn propose(h: &Harness, text: &str, key: &str) -> (i64, Value) {
    let run_id = h.add_run(key, "RUNNING", None);
    let role = if key == spec::IMPLEMENTER { "implementer" } else { "code_reviewer" };
    (run_id, h.engine.agent_lesson("TEST-1", role, Some(run_id), text).await.unwrap())
}

fn guidance(h: &Harness, name: &str) -> PathBuf {
    PathBuf::from(&h.load().task.repository_root).join(".agentflow-instructions").join(name)
}

fn snapshot_lessons(h: &Harness) -> Vec<Value> {
    h.engine.snapshot("TEST-1").unwrap()["lessons"].as_array().unwrap().clone()
}

fn uid(result: &Value) -> String {
    result["uid"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn test_an_agent_proposes_and_the_snapshot_shows_it() {
    let h = engine().await;
    let (run_id, result) = propose(&h, "Run   the build\nfirst.", spec::IMPLEMENTER).await;
    assert_eq!((&result["ok"], &result["count"]), (&json!(true), &json!(1)));
    let lessons = snapshot_lessons(&h);
    let lesson = &lessons[0];
    assert_eq!(
        (&lesson["role"], &lesson["nodeKey"], &lesson["runId"], &lesson["text"], &lesson["status"]),
        (
            &json!("implementer"),
            &json!(spec::IMPLEMENTER),
            &json!(run_id),
            &json!("Run the build first."),
            &json!("proposed")
        )
    );
    assert_eq!((&lesson["decidedAt"], &lesson["path"]), (&Value::Null, &Value::Null));
    assert!(!guidance(&h, "implementer.md").exists(), "a proposal writes nothing to the repository");
}

#[tokio::test]
async fn test_proposals_need_an_active_run_and_are_capped() {
    let h = engine().await;
    let done = h.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    let error = h.engine.agent_lesson("TEST-1", "implementer", Some(done), "Too late").await.unwrap_err();
    assert_eq!(error.status, 409);
    assert!(error.message.contains("active run"));
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    assert_eq!(h.engine.agent_lesson("TEST-1", "implementer", Some(run), " ").await.unwrap_err().status, 422);
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    for n in 0..lessons::MAX_PER_RUN {
        h.engine.agent_lesson("TEST-1", "implementer", Some(run), &format!("Lesson {n}")).await.unwrap();
    }
    let error = h.engine.agent_lesson("TEST-1", "implementer", Some(run), "One too many").await.unwrap_err();
    assert!(error.message.contains("already proposed 5 lessons"), "{}", error.message);
}

#[tokio::test]
async fn test_approving_appends_the_edited_text_to_the_original_checkout() {
    let h = engine().await;
    h.git.returns("dirty_paths", Outcome::Ok(json!(["?? .agentflow-instructions/"])));
    let (_, proposed) = propose(&h, "Run the build first", spec::IMPLEMENTER).await;
    let result = h
        .engine
        .decide_lesson("TEST-1", &uid(&proposed), true, Some("Run `npm run build`\nbefore the tests"))
        .await
        .unwrap();
    let path = guidance(&h, "implementer.md");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"## Lessons from past tasks\n\n- Run `npm run build` before the tests\n"
    );
    assert_eq!(result["dirtyPaths"], json!(["?? .agentflow-instructions/"]));
    let lesson = &result["lesson"];
    assert_eq!(
        (&lesson["status"], &lesson["approvedText"], &lesson["text"]),
        (&json!("approved"), &json!("Run `npm run build` before the tests"), &json!("Run the build first"))
    );
    assert_eq!(PathBuf::from(lesson["path"].as_str().unwrap()), path);
    assert!(lesson["decidedAt"].is_string());
    let worktree = PathBuf::from(&h.load().task.worktree_path);
    assert!(!worktree.join(".agentflow-instructions").exists());

    let error = h.engine.decide_lesson("TEST-1", &uid(&proposed), true, None).await.unwrap_err();
    assert_eq!(error.status, 409);
    assert!(error.message.contains("already approved"));
    assert_eq!(std::fs::read_to_string(&path).unwrap().matches("- Run").count(), 1);
}

#[tokio::test]
async fn test_approving_keeps_existing_guidance_and_its_line_endings() {
    let h = engine().await;
    let path = guidance(&h, "code-reviewer.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"# Reviewer\r\n\r\nBe brief.").unwrap();
    let (_, proposed) = propose(&h, "The lint warning in gen/ is expected", spec::CODE_REVIEW).await;
    h.engine.decide_lesson("TEST-1", &uid(&proposed), true, None).await.unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"# Reviewer\r\n\r\nBe brief.\r\n\r\n## Lessons from past tasks\r\n\r\n- The lint warning in gen/ is expected\r\n"
    );
}

#[tokio::test]
async fn test_dismissing_writes_nothing_and_unknown_lessons_are_404() {
    let h = engine().await;
    let (_, proposed) = propose(&h, "Not useful", spec::IMPLEMENTER).await;
    let result = h.engine.decide_lesson("TEST-1", &uid(&proposed), false, None).await.unwrap();
    assert_eq!((&result["lesson"]["status"], &result["dirtyPaths"]), (&json!("dismissed"), &json!([])));
    assert!(!guidance(&h, "implementer.md").exists());
    let error = h.engine.decide_lesson("TEST-1", &uid(&proposed), true, None).await.unwrap_err();
    assert!(error.message.contains("already dismissed"));
    let missing = h.engine.decide_lesson("TEST-1", "00000000-0000-0000-0000-000000000000", true, None).await;
    assert_eq!(missing.unwrap_err().status, 404);
}

#[tokio::test]
async fn test_guidance_that_cannot_take_the_lesson_is_refused() {
    let h = engine().await;
    let path = guidance(&h, "implementer.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "x".repeat(64 * 1024 - 10)).unwrap();
    let (_, proposed) = propose(&h, "This would make the file too large", spec::IMPLEMENTER).await;
    assert_eq!(h.engine.decide_lesson("TEST-1", &uid(&proposed), true, None).await.unwrap_err().status, 409);
    assert_eq!(snapshot_lessons(&h)[0]["status"], "proposed");
}

#[tokio::test]
async fn test_an_unreadable_lessons_file_fails_closed() {
    let h = engine().await;
    propose(&h, "First", spec::IMPLEMENTER).await;
    let store = lessons::path(&PathBuf::from(h.engine.snapshot("TEST-1").unwrap()["taskDir"].as_str().unwrap()));
    std::fs::write(&store, "{not json").unwrap();
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    let error = h.engine.agent_lesson("TEST-1", "implementer", Some(run), "Second").await.unwrap_err();
    assert_eq!(error.status, 409);
    assert!(error.message.contains("unreadable"), "{}", error.message);
    assert_eq!(std::fs::read_to_string(&store).unwrap(), "{not json", "never replaced");
    assert!(snapshot_lessons(&h).is_empty());
}
