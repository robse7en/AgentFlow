//! Decision-tree agent questions (`engine::decisions`), ported from
//! backend/tests/test_decision_questions.py: validation, the answer line, the engine and Slack.

mod engine_support;

use agentflow_core::chat::{answers, models::AnswerPrompt, models::AnswerRequest};
use agentflow_core::config::SlackConfig;
use agentflow_core::engine::agents::clean_ask;
use agentflow_core::engine::decisions::{ANSWER_SEPARATOR, answer_line, source_allowed};
use agentflow_core::engine::spec;
use engine_support::{Harness, add_session, engine, tab_seven};
use serde_json::{Value, json};

fn tree() -> Value {
    json!({
        "message": "312 inactive accounts still own open invoices. What should the cleanup do?",
        "context": " 4,108 accounts inactive · billing reads invoice.owner_id ",
        "risk": "high",
        "choices": [
            {"label": "Soft-delete, keep invoices", "recommended": true, "followUp": {
                "message": "When are they purged for good?",
                "choices": [
                    {"label": "After their invoices close", "recommended": true, "detail": "A nightly job purges"},
                    {"label": "Never", "warning": "Table grows ~4k rows a year",
                     "note": "Legal asked for a purge date.", "source": "docs/privacy.md"},
                ]}},
            {"label": "Skip accounts with open invoices", "detail": "", "recommended": false},
            {"label": "Hard-delete everything", "warning": "Breaks history on 312 invoices",
             "note": "Billing reads owner_id.", "source": "https://example.com/billing#owner"},
        ],
    })
}

fn ask(data: &Value) -> Result<(String, Vec<String>, Option<Value>), String> {
    clean_ask(data["message"].as_str().unwrap(), data.get("choices"), data.get("context"), data.get("risk"))
}

/// `tree()` with `fields` merged into the object at `path` (keys and indexes).
fn changed(path: &[Value], fields: Value) -> Value {
    let mut data = tree();
    let mut target = &mut data;
    for step in path {
        target = match step {
            Value::String(key) => &mut target[key.as_str()],
            Value::Number(index) => &mut target[index.as_u64().unwrap() as usize],
            _ => unreachable!(),
        };
    }
    for (key, value) in fields.as_object().unwrap() {
        target[key.as_str()] = value.clone();
    }
    data
}

#[test]
fn a_tree_is_stored_as_validated_with_its_top_level_labels_as_choices() {
    let (message, choices, decision) = ask(&tree()).unwrap();
    assert!(message.starts_with("312 inactive"));
    assert_eq!(choices, ["Soft-delete, keep invoices", "Skip accounts with open invoices", "Hard-delete everything"]);
    assert_eq!(
        decision.unwrap(),
        json!({
            "context": "4,108 accounts inactive · billing reads invoice.owner_id",
            "risk": "high",
            "choices": [
                {"label": "Soft-delete, keep invoices", "recommended": true, "followUp": {
                    "message": "When are they purged for good?",
                    "choices": [
                        {"label": "After their invoices close", "recommended": true, "detail": "A nightly job purges"},
                        {"label": "Never", "warning": "Table grows ~4k rows a year",
                         "note": "Legal asked for a purge date.", "source": "docs/privacy.md"},
                    ]}},
                {"label": "Skip accounts with open invoices"},
                {"label": "Hard-delete everything", "warning": "Breaks history on 312 invoices",
                 "note": "Billing reads owner_id.", "source": "https://example.com/billing#owner"},
            ],
        })
    );
}

#[test]
fn flat_questions_keep_their_shape_and_context_or_risk_alone_make_a_tree() {
    let flat = ask(&json!({"message": " Which rollout? ", "choices": [" Gradual ", "All at once"]})).unwrap();
    assert_eq!(flat, ("Which rollout?".into(), vec!["Gradual".into(), "All at once".into()], None));
    assert_eq!(ask(&json!({"message": "Free text only"})).unwrap().2, None);
    let bare = ask(&json!({"message": "Anything else?", "risk": "low"})).unwrap();
    assert_eq!(bare.2, Some(json!({"context": null, "risk": "low", "choices": []})));
    assert!(ask(&json!({"message": "Pick", "choices": ["Same", "same"]})).unwrap_err().contains("must be unique"));
}

#[test]
fn every_rule_rejects_with_the_field_at_fault() {
    let pair = || json!({"message": "Then?", "choices": [{"label": "A"}, {"label": "B"}]});
    let deeper = json!({"message": "Deeper?", "choices": [{"label": "A"}, {"label": "B"}]});
    let follow = |i: u64| vec![json!("choices"), json!(0), json!("followUp"), json!("choices"), json!(i)];
    let choice = |i: u64| vec![json!("choices"), json!(i)];
    let cases: Vec<(Value, &str)> = vec![
        (changed(&[], json!({"choices": ["Plain", {"label": "Object"}]})), "choices: use objects for every choice"),
        (changed(&[], json!({"risk": "severe"})), "risk: must be low, medium or high"),
        (changed(&[], json!({"risk": 3})), "risk: must be low, medium or high"),
        (changed(&[], json!({"context": "line\nbreak"})), "context: must be a single line"),
        (changed(&[], json!({"context": "x".repeat(501)})), "context: must be at most 500 characters"),
        (
            changed(&[], json!({"choices": (0..9).map(|n| json!({"label": n.to_string()})).collect::<Vec<_>>()})),
            "choices: at most 8 choices",
        ),
        (changed(&choice(1), json!({"label": " "})), "choices[1].label: cannot be empty"),
        (changed(&choice(1), json!({"label": 7})), "choices[1].label: must be a string"),
        (changed(&choice(1), json!({"label": "x".repeat(251)})), "choices[1].label: must be at most 250 characters"),
        (
            changed(&choice(1), json!({"label": format!("A{ANSWER_SEPARATOR}B")})),
            "choices[1].label: cannot contain \u{2192}",
        ),
        (changed(&choice(1), json!({"label": "A\u{2192}B"})), "choices[1].label: cannot contain \u{2192}"),
        (changed(&choice(1), json!({"label": "Hard-delete EVERYTHING"})), "choices: labels must be unique"),
        (changed(&choice(1), json!({"recommended": true})), "choices: at most one choice can be recommended"),
        (changed(&choice(1), json!({"recommended": "yes"})), "choices[1].recommended: must be true or false"),
        (changed(&choice(1), json!({"detail": "x".repeat(201)})), "choices[1].detail: must be at most 200 characters"),
        (
            changed(&choice(1), json!({"warning": "x".repeat(121)})),
            "choices[1].warning: must be at most 120 characters",
        ),
        (changed(&choice(1), json!({"note": "x".repeat(301)})), "choices[1].note: must be at most 300 characters"),
        (changed(&choice(1), json!({"source": "docs/a.md"})), "choices[1].source: needs a note"),
        (changed(&choice(2), json!({"source": "x".repeat(201)})), "choices[2].source: must be at most 200 characters"),
        (changed(&choice(1), json!({"warn": "typo"})), "choices[1]: unknown field 'warn'"),
        (
            changed(&choice(1), json!({"followUp": {"message": "Then?", "choices": [{"label": "Only one"}]}})),
            "choices[1].followUp.choices: must have 2 to 8 choices",
        ),
        (
            changed(&choice(1), json!({"followUp": {"message": " ", "choices": [{"label": "A"}, {"label": "B"}]}})),
            "choices[1].followUp.message: cannot be empty",
        ),
        (
            changed(
                &choice(1),
                json!({"followUp": {"message": "x".repeat(501), "choices": [{"label": "A"}, {"label": "B"}]}}),
            ),
            "choices[1].followUp.message: must be at most 500 characters",
        ),
        (
            changed(&choice(1), json!({"followUp": {"risk": "low", "message": "Then?", "choices": []}})),
            "choices[1].followUp: unknown field 'risk'",
        ),
        (changed(&choice(1), json!({"followUp": "Then?"})), "choices[1].followUp: must be an object"),
        (changed(&choice(1), json!({"followUp": pair()["choices"].clone()})), "choices[1].followUp: must be an object"),
        (
            changed(&follow(1), json!({"followUp": deeper})),
            "choices[0].followUp.choices[1].followUp: follow-up options cannot ask another follow-up",
        ),
        (
            changed(&follow(1), json!({"recommended": true})),
            "choices[0].followUp.choices: at most one choice can be recommended",
        ),
        (
            changed(&follow(1), json!({"label": "after their invoices close"})),
            "choices[0].followUp.choices: labels must be unique",
        ),
        (changed(&follow(1), json!({"note": null})), "choices[0].followUp.choices[1].source: needs a note"),
    ];
    for (data, problem) in cases {
        let error = ask(&data).unwrap_err();
        assert!(error.contains(problem), "{error:?} should contain {problem:?}");
    }
}

#[test]
fn a_source_is_a_relative_path_or_an_https_url() {
    for (source, allowed) in [
        ("docs/privacy.md", true),
        ("src\\billing\\models.py", true),
        ("README.md", true),
        ("https://example.com/policy", true),
        ("../secrets.txt", false),
        ("docs/../../etc/passwd", false),
        ("docs\\..\\x", false),
        ("/etc/passwd", false),
        ("\\\\server\\share", false),
        ("C:/Windows/win.ini", false),
        ("c:x", false),
        ("http://example.com", false),
        ("file:///etc/passwd", false),
        ("https://", false),
        ("https://example.com/a b", false),
    ] {
        assert_eq!(source_allowed(source), allowed, "{source}");
        let data = changed(&[json!("choices"), json!(1)], json!({"note": "Evidence.", "source": source}));
        match ask(&data) {
            Ok((_, _, decision)) => assert!(allowed && decision.unwrap()["choices"][1]["source"] == source, "{source}"),
            Err(error) => {
                assert!(!allowed && error.contains("must be a repository-relative path"), "{source}: {error}")
            }
        }
    }
}

#[test]
fn answer_lines_for_paths_free_text_and_flat_questions() {
    let (_, labels, decision) = ask(&tree()).unwrap();
    let decision = decision.as_ref();
    assert_eq!(answer_line(&labels, decision, Some(&[2]), None).unwrap(), "3: Hard-delete everything");
    assert_eq!(
        answer_line(&labels, decision, Some(&[0, 0]), None).unwrap(),
        "1.1: Soft-delete, keep invoices \u{2192} After their invoices close"
    );
    assert_eq!(answer_line(&labels, decision, None, Some("Ask legal first")).unwrap(), "other: Ask legal first");
    let flat = ["Gradual".to_string(), "All at once".to_string()];
    assert_eq!(answer_line(&flat, None, Some(&[1]), None).unwrap(), "All at once");
    assert_eq!(answer_line(&flat, None, None, Some("Something else")).unwrap(), "Something else");
    assert_eq!(answer_line(&flat, None, Some(&[0, 0]), None).unwrap_err(), "That choice does not exist");

    assert_eq!(
        answer_line(&labels, decision, Some(&[0]), None).unwrap_err(),
        "That choice has a follow-up question; pick one of its options"
    );
    for path in [&[3][..], &[0, 2], &[1, 0], &[0, 0, 0], &[]] {
        assert_eq!(
            answer_line(&labels, decision, Some(path), None).unwrap_err(),
            "That choice does not exist",
            "{path:?}"
        );
    }
}

#[test]
fn the_longest_path_line_fits_in_one_answer() {
    let labels: Vec<String> = (0..8).map(|n| format!("{n}{}", "x".repeat(249))).collect();
    let options: Vec<Value> = labels.iter().map(|l| json!({"label": l})).collect();
    let choices: Vec<Value> =
        labels.iter().map(|l| json!({"label": l, "followUp": {"message": "Then?", "choices": options}})).collect();
    let (_, labels, decision) = ask(&json!({"message": "Long?", "choices": choices})).unwrap();
    let line = answer_line(&labels, decision.as_ref(), Some(&[7, 7]), None).unwrap();
    assert!(line.starts_with("8.8: "));
    assert_eq!(line.chars().count(), 5 + 250 + 3 + 250);
    assert_eq!(agentflow_core::engine::agents::clean_answer(&line).unwrap(), line);
}

// ------------------------------------------------------------------------------ the engine

async fn waiting() -> (Harness, i64) {
    let h = engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&h, run);
    tab_seven(&h);
    (h, run)
}

async fn asked(h: &Harness, run: i64, data: &Value) -> Value {
    let (message, choices, decision) = ask(data).unwrap();
    h.engine.agent_decision("TEST-1", "implementer", Some(run), &message, &choices, decision).await.unwrap();
    h.engine.snapshot("TEST-1").unwrap()["questions"].as_array().unwrap().last().unwrap().clone()
}

fn writes(h: &Harness) -> Vec<String> {
    h.terminals.calls().into_iter().filter(|c| c.starts_with("write_line")).collect()
}

fn node_state(h: &Harness) -> String {
    h.load().node(spec::IMPLEMENTER).unwrap().state.clone()
}

#[tokio::test]
async fn a_path_answer_types_the_built_line_and_is_idempotent() {
    let (h, run) = waiting().await;
    let question = asked(&h, run, &tree()).await;
    let uid = question["uid"].as_str().unwrap();
    assert_eq!(question["decision"]["risk"], "high");
    for path in [&[0][..], &[5]] {
        let error = h.engine.answer_question_path("TEST-1", uid, path, None).await.unwrap_err();
        assert_eq!(error.status, 422, "{path:?}");
    }
    assert!(writes(&h).is_empty());

    let snapshot = h.engine.answer_question_path("TEST-1", uid, &[0, 1], None).await.unwrap();
    let line = format!("1.2: Soft-delete, keep invoices{ANSWER_SEPARATOR}Never");
    assert_eq!(writes(&h), [format!("write_line agentflow-TEST-1 terminal_7 {line}")]);
    assert_eq!(snapshot["questions"][0]["answer"], line.as_str());
    assert_eq!(node_state(&h), "RUNNING");

    h.engine.answer_question_path("TEST-1", uid, &[0, 1], None).await.unwrap();
    assert_eq!(writes(&h).len(), 1);
    for again in [
        h.engine.answer_question_path("TEST-1", uid, &[2], None).await,
        h.engine.answer_question("TEST-1", uid, "Never", None).await,
    ] {
        assert!(again.unwrap_err().message.contains("already been answered"));
    }
}

#[tokio::test]
async fn free_text_on_a_tree_gets_the_other_prefix_within_the_answer_limit() {
    let (h, run) = waiting().await;
    let uid = asked(&h, run, &tree()).await["uid"].as_str().unwrap().to_string();
    let error = h.engine.answer_question("TEST-1", &uid, &"x".repeat(3994), None).await.unwrap_err();
    assert!(error.message.contains("at most 4000 characters"), "{error}");
    h.engine.answer_question("TEST-1", &uid, &format!("  {}  ", "x".repeat(3993)), None).await.unwrap();
    assert_eq!(writes(&h), [format!("write_line agentflow-TEST-1 terminal_7 other: {}", "x".repeat(3993))]);
}

#[tokio::test]
async fn flat_questions_accept_a_path_and_answer_with_the_choice_text() {
    let (h, run) = waiting().await;
    let question = asked(&h, run, &json!({"message": "Which rollout?", "choices": ["Gradual", "All at once"]})).await;
    assert!(question["decision"].is_null());
    let uid = question["uid"].as_str().unwrap();
    h.engine.answer_question_path("TEST-1", uid, &[1], None).await.unwrap();
    h.engine.answer_question("TEST-1", uid, "All at once", None).await.unwrap();
    assert_eq!(writes(&h), ["write_line agentflow-TEST-1 terminal_7 All at once"]);
}

#[tokio::test]
async fn the_node_keeps_waiting_until_the_last_of_several_answers() {
    let (h, run) = waiting().await;
    let first = asked(&h, run, &tree()).await;
    let second = asked(
        &h,
        run,
        &json!({"message": "Where should it run?", "risk": "low", "choices": [
        {"label": "Nightly worker"}, {"label": "New job", "followUp": {"message": "How often?", "choices": [
            {"label": "Weekly", "recommended": true}, {"label": "Daily"}]}}]}),
    )
    .await;
    let node = |snapshot: &Value| {
        snapshot["nodes"].as_array().unwrap().iter().find(|n| n["key"] == spec::IMPLEMENTER).cloned().unwrap()
    };

    let snapshot = h.engine.answer_question_path("TEST-1", first["uid"].as_str().unwrap(), &[2], None).await.unwrap();
    assert_eq!(
        (node(&snapshot)["state"].clone(), node(&snapshot)["openQuestions"].clone()),
        (json!("WAITING_FOR_USER"), json!(1))
    );
    let open: Vec<Value> = snapshot["questions"].as_array().unwrap().iter().map(|q| q["actionable"].clone()).collect();
    assert_eq!(open, [json!(false), json!(true)]);
    assert_eq!(h.engine.open_questions().unwrap().len(), 1);

    let snapshot =
        h.engine.answer_question_path("TEST-1", second["uid"].as_str().unwrap(), &[1, 0], None).await.unwrap();
    assert_eq!(
        (node(&snapshot)["state"].clone(), node(&snapshot)["openQuestions"].clone()),
        (json!("RUNNING"), json!(0))
    );
    assert_eq!(snapshot["questions"][1]["answer"], format!("2.1: New job{ANSWER_SEPARATOR}Weekly"));
    assert!(h.engine.open_questions().unwrap().is_empty());
}

#[tokio::test]
async fn the_inbox_lists_a_waiting_question_with_its_task_and_agent() {
    let (h, run) = waiting().await;
    let question = asked(&h, run, &tree()).await;
    let listed = h.engine.open_questions().unwrap();
    assert_eq!(listed.len(), 1);
    let item = &listed[0];
    assert_eq!(item["uid"], question["uid"]);
    assert_eq!(item["decision"], question["decision"]);
    assert_eq!(
        (item["jiraKey"].clone(), item["nodeKey"].clone(), item["actionable"].clone()),
        (json!("TEST-1"), json!("implementer"), json!(true))
    );
    assert_eq!(item["nodeLabel"], h.load().node(spec::IMPLEMENTER).unwrap().label.as_str());
    for field in ["jiraTitle", "harness", "model", "createdAt", "runId", "role", "message", "choices"] {
        assert!(item.get(field).is_some(), "{field}");
    }
    h.set_status("ACTIVE");
    h.with_task(|state| state.task.archived_at = Some(agentflow_core::db::DbTime::now()));
    assert!(h.engine.open_questions().unwrap().is_empty(), "archived tasks are not listed");
}

#[tokio::test]
async fn the_answer_route_takes_free_text_or_a_path() {
    let (h, run) = waiting().await;
    let uid = asked(&h, run, &tree()).await["uid"].as_str().unwrap().to_string();
    let url = format!("/api/tasks/TEST-1/questions/{uid}/answer");
    for body in [
        json!({}),
        json!({"answer": "x", "path": [0]}),
        json!({"path": []}),
        json!({"path": [0, 1, 2]}),
        json!({"path": [-1]}),
        json!({"path": ["1"]}),
        json!({"path": [true]}),
        json!({"path": [0.5]}),
        json!({"path": [0]}),
        json!({"path": [9]}),
    ] {
        assert_eq!(h.route("POST", &url, Some(body.clone())).await.0, 422, "{body}");
    }
    let (status, snapshot) = h.route("POST", &url, Some(json!({"path": [0, 0]}))).await;
    assert_eq!(status, 200);
    assert_eq!(
        snapshot["questions"][0]["answer"],
        format!("1.1: Soft-delete, keep invoices{ANSWER_SEPARATOR}After their invoices close")
    );
    assert_eq!(h.route("GET", "/api/questions?open=true", None).await, (200, json!([])));
    assert_eq!(h.route("GET", "/api/questions?open=false", None).await.0, 422);
}

// ------------------------------------------------------------------------------ Slack, two steps

fn slack_request(uid: &str) -> AnswerRequest {
    AnswerRequest {
        user_id: "U-operator".into(),
        channel: "D-operator".into(),
        jira_key: "TEST-1".into(),
        question_uid: uid.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_slack_branch_shows_its_follow_up_and_a_leaf_sends_the_path() {
    let (h, run) = waiting().await;
    let question = asked(&h, run, &tree()).await;
    let uid = question["uid"].as_str().unwrap();
    let settings = SlackConfig { user_id: Some("U-operator".into()), ..Default::default() };
    let respond = |request: AnswerRequest| {
        let (engine, settings) = (h.engine.clone(), settings.clone());
        async move { answers::respond(&engine, &settings, Some("D-operator"), &request).await }
    };
    let prompt = AnswerPrompt {
        jira_key: "TEST-1".into(),
        question_uid: uid.into(),
        choices: vec![
            "Soft-delete, keep invoices".into(),
            "Skip accounts with open invoices".into(),
            "Hard-delete everything".into(),
        ],
        decision: Some(question["decision"].clone()),
        branch: None,
    };
    let shown = respond(AnswerRequest { choice_index: Some(0), ..slack_request(uid) }).await.unwrap().unwrap();
    assert_eq!(shown, AnswerPrompt { branch: Some(0), ..prompt.clone() });
    assert_eq!(respond(AnswerRequest { back: true, ..slack_request(uid) }).await.unwrap(), Some(prompt));
    for path in [(0, 5), (9, 0)] {
        assert_eq!(
            respond(AnswerRequest { path: Some(path), ..slack_request(uid) }).await.unwrap_err(),
            "That choice is no longer offered."
        );
    }
    assert!(writes(&h).is_empty());

    assert_eq!(respond(AnswerRequest { path: Some((0, 0)), ..slack_request(uid) }).await, Ok(None));
    let line = format!("1.1: Soft-delete, keep invoices{ANSWER_SEPARATOR}After their invoices close");
    assert_eq!(writes(&h), [format!("write_line agentflow-TEST-1 terminal_7 {line}")]);
    assert!(
        h.engine.snapshot("TEST-1").unwrap()["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["message"].as_str().unwrap_or("").ends_with("via Slack"))
    );
    assert_eq!(
        respond(AnswerRequest { choice_index: Some(0), ..slack_request(uid) }).await.unwrap_err(),
        "That question is no longer open."
    );
}

#[tokio::test]
async fn a_slack_leaf_branch_and_free_text_on_a_tree() {
    let (h, run) = waiting().await;
    let settings = SlackConfig { user_id: Some("U-operator".into()), ..Default::default() };
    let first = asked(&h, run, &tree()).await;
    let second = asked(&h, run, &tree()).await;
    let request =
        AnswerRequest { text: Some("Ask legal first".into()), ..slack_request(first["uid"].as_str().unwrap()) };
    assert_eq!(answers::respond(&h.engine, &settings, Some("D-operator"), &request).await, Ok(None));
    let request = AnswerRequest { choice_index: Some(2), ..slack_request(second["uid"].as_str().unwrap()) };
    assert_eq!(answers::respond(&h.engine, &settings, Some("D-operator"), &request).await, Ok(None));
    assert_eq!(
        writes(&h),
        [
            "write_line agentflow-TEST-1 terminal_7 other: Ask legal first",
            "write_line agentflow-TEST-1 terminal_7 3: Hard-delete everything"
        ]
    );
}

#[tokio::test]
async fn a_slack_tree_answered_in_the_browser_is_closed_for_slack() {
    let (h, run) = waiting().await;
    let settings = SlackConfig { user_id: Some("U-operator".into()), ..Default::default() };
    let uid = asked(&h, run, &tree()).await["uid"].as_str().unwrap().to_string();
    h.engine.answer_question_path("TEST-1", &uid, &[2], None).await.unwrap();
    for request in [
        AnswerRequest { path: Some((0, 0)), ..slack_request(&uid) },
        AnswerRequest { choice_index: Some(2), ..slack_request(&uid) },
    ] {
        assert_eq!(
            answers::respond(&h.engine, &settings, Some("D-operator"), &request).await.unwrap_err(),
            "That question is no longer open."
        );
    }
    assert_eq!(writes(&h).len(), 1);
}
