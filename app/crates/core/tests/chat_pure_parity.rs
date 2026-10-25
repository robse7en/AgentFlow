use agentflow_core::chat::{
    actions, details, forms,
    models::*,
    notices::{self, Notice},
    render, rich,
};
use serde_json::{Value, json};
fn detail(snapshot: &Value, allowed: &[&str]) -> Value {
    details::task_details(snapshot, &allowed.iter().map(|s| (*s).into()).collect::<Vec<_>>(), None, 10)
}
#[test]
fn test_untrusted_agent_text_cannot_mention_or_link() {
    let notice = Notice {
        summary: "Implementer asked a question".into(),
        question: Some("Ping <!channel> via <http://evil.example|here> & done".into()),
        choices: vec!["<@U1>".into(), "a`b".into()],
        ..Default::default()
    };
    let text = render::render_notice(&notice, false);
    for raw in ["<!channel>", "<http", "<@U1>"] {
        assert!(!text.contains(raw));
    }
    assert!(text.contains("&lt;!channel&gt;"));
    assert!(text.contains("&amp; done"));
    assert!(text.contains("`a'b`"));
}
#[test]
fn test_notify_setting_filters_conditions() {
    let snapshot = json!({"jiraKey":"K-1","status":"READY_FOR_PR","questions":[],"attention":[{"nodeKey":"x","label":"X","state":"READY"}]});
    assert_eq!(
        notices::notices(&snapshot, &["READY_FOR_PR".into()], true).keys().collect::<Vec<_>>(),
        ["status:READY_FOR_PR"]
    );
    assert_eq!(notices::notices(&snapshot, &["READY".into()], true).keys().collect::<Vec<_>>(), ["node:x:READY"]);
}
#[test]
fn test_list_hides_finished_tasks_and_caps_length() {
    let mut tasks: Vec<_> = (0..30)
        .map(|i| json!({"jiraKey":format!("K-{i}"),"status":"ACTIVE","phase":"Architect: RUNNING","attention":[]}))
        .collect();
    tasks.push(json!({"jiraKey":"DONE-1","status":"MERGED"}));
    let text = render::render_list(&tasks);
    assert!(!text.contains("DONE-1"));
    assert!(text.ends_with("…and 5 more"));
    assert_eq!(render::render_list(&tasks[30..]), "No active AgentFlow tasks.");
}
#[test]
fn test_disallowed_sections_are_never_read() {
    let snapshot = json!({"statusMessage":"SECRET","requirements":{"requirements":"SECRET"},"findings":[{"title":"SECRET","status":"open"}],"events":[{"message":"SECRET"}],"prUrl":"https://x/SECRET","jiraIssue":{"markdown":"SECRET"},"totalUsage":{"runs":1,"billedCostUsd":1.5}});
    let result = detail(&snapshot, &["cost"]);
    assert_eq!(result.as_object().unwrap().keys().collect::<Vec<_>>(), ["cost"]);
    assert!(!result.to_string().contains("SECRET"));
    assert_eq!(
        details::task_details(&snapshot, &["cost".into(), "errors".into()], Some(&["errors".into()]), 10)
            .as_object()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn test_pr_and_ticket_details_are_trimmed_and_escaped() {
    let description = (0..40).map(|i| format!("step {i} <!channel>")).collect::<Vec<_>>().join("\n");
    let comments = (1..=5)
        .map(|i| format!("### Comment {i} - Author {i}, 2026-09-0{i} 10:00\n\nPing <!here> {}", "x".repeat(900)))
        .collect::<Vec<_>>()
        .join("\n\n");
    let snapshot = json!({"jiraKey":"K-1","phase":"READY_FOR_PR","prUrl":"https://github.com/o/r/pull/7","prCreatedAt":"2026-09-01T10:00:00+00:00","prSyncPaused":true,"repositoryRoot":"/secret","jiraIssue":{"markdown":format!("# K-1: Title\n\n## Description\n\n{description}\n\n## Comments (5)\n\n{comments}"),"syncedAt":"2026-09-02T08:30:00+00:00"}});
    let d = detail(&snapshot, &["pr", "ticket"]);
    assert!(!d.to_string().contains("/secret"));
    assert!(d["ticket"]["description"].as_str().unwrap().contains("step 14"));
    assert!(!d["ticket"]["description"].as_str().unwrap().contains("step 15"));
    assert_eq!(d["ticket"]["comments"]["count"], 5);
    assert_eq!(d["ticket"]["comments"]["latest"].as_array().unwrap().len(), 3);
    let text = rich::render_details(&snapshot, &d, "u");
    assert!(text.contains("&lt;!here&gt;"));
    assert!(!text.contains("<!channel>"));
    assert!(text.contains("GitHub PR sync is paused"));
}
#[test]
fn test_pr_and_ticket_details_say_when_there_is_nothing_yet() {
    let empty = json!({"jiraKey":"K-1","phase":"ACTIVE"});
    let text = rich::render_details(&empty, &detail(&empty, &["pr", "ticket"]), "u");
    assert!(text.contains("None recorded yet."));
    assert!(text.contains("Not synced from Jira yet."));
    let snapshot = json!({"jiraIssue":{"markdown":"# K-1: T\n\n## Description\n\n\n\n## Comments\n\nNot fetched (HTTP 403) - read them through your Jira MCP.\n"}});
    let text = rich::render_details(&snapshot, &detail(&snapshot, &["ticket"]), "u");
    assert!(text.contains("Comments were not synced."));
    assert!(!text.contains("MCP"));
}
#[test]
fn test_ticket_details_read_what_the_jira_sync_writes() {
    let fields = json!({"summary":"Title"});
    let rendered = json!({"description":"<p>First line</p><p>Second line</p>"});
    let comments:Vec<_>=(1..=4).map(|i|json!({"id":i.to_string(),"author":{"displayName":format!("Dev {i}")},"created":"2026-09-01T10:00:00.000+0000","renderedBody":format!("<p>Comment {i}</p>")})).collect();
    let md =
        agentflow_core::jira::issue_markdown("K-1", &fields, &rendered, Some(&comments), &Default::default(), None);
    let d = detail(&json!({"jiraIssue":{"markdown":md}}), &["ticket"]);
    assert_eq!(d["ticket"]["description"], "First line\nSecond line");
    assert_eq!(d["ticket"]["comments"]["count"], 4);
    assert_eq!(d["ticket"]["comments"]["latest"][0]["excerpt"], "Comment 2");
    let md =
        agentflow_core::jira::issue_markdown("K-1", &fields, &rendered, None, &Default::default(), Some("HTTP 403"));
    assert!(detail(&json!({"jiraIssue":{"markdown":md}}), &["ticket"])["ticket"]["comments"].is_null());
    let legacy = detail(
        &json!({"jiraIssue":{"markdown":"# K-1: Title\n\nRelease Instructions\n\nFollow the normal release cycle.\n"}}),
        &["ticket"],
    );
    assert_eq!(legacy["ticket"]["description"], "Release Instructions\nFollow the normal release cycle.");
}
#[test]
fn test_details_render_untrusted_text_escaped_and_trimmed() {
    let mut findings = vec![
        json!({"status":"open","title":"Ping <!channel>","severity":"high","blocking":true,"file":"a`b.py","line":3}),
    ];
    findings.extend((0..20).map(|i| json!({"status":"open","title":format!("minor {i}"),"severity":"low"})));
    findings.push(json!({"status":"resolved","title":"gone"}));
    let snapshot = json!({"jiraKey":"K-1","phase":"Code Review: FAILED","findings":findings,"nodes":[{"label":"Code Review","error":(0..50).map(|i|format!("line {i} <http://x|y>")).collect::<Vec<_>>().join("\n")}],"totalUsage":{"runs":2,"billedCostUsd":1.25,"estimatedCostUsd":3.5,"totalTokens":12000},"budget":{"task":{"knownUsd":1.25,"limitUsd":10,"unknownRuns":0}}});
    let text =
        rich::render_details(&snapshot, &detail(&snapshot, &["findings", "errors", "cost"]), "http://ui/tasks/K-1");
    assert!(text.contains("&lt;!channel&gt;"));
    assert!(text.contains("`a'b.py:3`"));
    assert!(text.contains("…and 6 more"));
    assert!(!text.contains("gone"));
    assert!(text.contains("line 5"));
    assert!(!text.contains("line 6"));
    assert!(text.contains("billed $1.25 · estimated $3.50 · 12,000 tokens"));
    assert!(text.ends_with("<http://ui/tasks/K-1|Open in AgentFlow>"));
}
#[test]
fn test_typed_slack_text_reaches_agentflow_as_written() {
    for (input, expected) in [
        ("a &lt; b &amp;&amp; c &gt; d", "a < b && c > d"),
        ("see <https://x.io/a?b=1&amp;c=2|x.io> and <https://y.io>", "see x.io and https://y.io"),
        ("<@U123> <!here> <#C1|general>", "@U123 here general"),
        ("&amp;lt;", "&lt;"),
    ] {
        assert_eq!(render::unslack(input), expected);
    }
}
#[test]
fn test_offer_buttons_carry_the_text_and_refuse_what_does_not_fit() {
    let offer = AnswerOffer { jira_key: "TEST-1".into(), question_uid: "u".repeat(36), text: "x | y".into() };
    let blocks = render::offer_blocks(&offer);
    let value = blocks.last().unwrap()["elements"][0]["value"].as_str().unwrap();
    assert_eq!(render::parse_offer(value), Some(("TEST-1", offer.question_uid.as_str(), "x | y")));
    let too_long = render::offer_blocks(&AnswerOffer { text: "z".repeat(1990), ..offer });
    assert_eq!(too_long.len(), 1);
    assert_eq!(too_long[0]["type"], "section");
}
#[test]
fn test_a_reply_renders_its_text_then_its_buttons_then_the_offer() {
    let reply = Reply {
        text: "No AgentFlow task QUO.".into(),
        buttons: vec![actions::button("cancel", "Cancel TEST-1", "TEST-1", Some("Sure?".into()))],
        offer: Some(AnswerOffer { jira_key: "TEST-1".into(), question_uid: "u".into(), text: "cancel it".into() }),
    };
    let blocks = render::reply_blocks(&reply).unwrap();
    assert_eq!(
        blocks.iter().map(|b| b["type"].as_str().unwrap()).collect::<Vec<_>>(),
        ["section", "actions", "section", "actions"]
    );
    assert_eq!(blocks[1]["elements"][0]["style"], "danger");
    assert_eq!(blocks[3]["elements"][0]["value"], "TEST-1|u|cancel it");
    assert!(render::reply_blocks(&Reply::text("short")).is_none());
}
#[test]
fn test_task_form_fits_slack_limits() {
    let long_path = format!("D:/{}repo", "deep/".repeat(40));
    let modal = forms::task_modal(&TaskForm {
        jira_key: Some("ABC-12".into()),
        repository: Some(long_path),
        search_tickets: true,
        implementation_mode: "parallel".into(),
        base_branch: "main".into(),
    });
    assert!(modal["title"]["text"].as_str().unwrap().chars().count() <= 24);
    assert_eq!(modal["blocks"].as_array().unwrap().len(), 6);
    for b in modal["blocks"].as_array().unwrap() {
        if let Some(option) = b["element"].get("initial_option") {
            assert!(option["text"]["text"].as_str().unwrap().chars().count() <= 75);
            assert!(option["value"].as_str().unwrap().chars().count() <= 150);
        }
    }
}
