//! Detailed replies retain the allowlist established by `details` and escape every data field.
use super::{
    details,
    render::{self, escape},
};
use serde_json::Value;
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn q(text: &str) -> String {
    let lines = crate::engine::prompts::py_splitlines(text);
    if lines.is_empty() {
        "> ".into()
    } else {
        lines.iter().map(|s| format!("> {}", escape(s))).collect::<Vec<_>>().join("\n")
    }
}
fn code(text: &str) -> String {
    format!("`{}`", escape(text).replace('`', "'"))
}
pub fn money(value: &Value) -> String {
    let Some(value) = value.as_f64() else {
        return "unknown".into();
    };
    let fixed = format!("{value:.2}");
    let (whole, cents) = fixed.split_once('.').unwrap();
    let sign = if whole.starts_with('-') { "-" } else { "" };
    let digits = whole.trim_start_matches('-');
    let number: String = digits
        .chars()
        .enumerate()
        .flat_map(|(i, c)| if i > 0 && (digits.len() - i).is_multiple_of(3) { vec![',', c] } else { vec![c] })
        .collect();
    format!("${sign}{number}.{cents}")
}
fn usage(value: &Value) -> String {
    let runs = value["runs"].as_u64().unwrap_or(0);
    let mut parts = vec![format!("{runs} run{}", if runs == 1 { "" } else { "s" })];
    for (field, label) in [("billedCostUsd", "billed"), ("estimatedCostUsd", "estimated")] {
        if !value[field].is_null() {
            parts.push(format!("{label} {}", money(&value[field])));
        }
    }
    if let Some(tokens) = value["totalTokens"].as_u64().filter(|t| *t > 0) {
        let digits = tokens.to_string();
        let count: String = digits
            .chars()
            .enumerate()
            .flat_map(|(i, c)| if i > 0 && (digits.len() - i).is_multiple_of(3) { vec![',', c] } else { vec![c] })
            .collect();
        parts.push(format!("{count} tokens"));
    }
    parts.join(" · ")
}
pub fn spend(label: &str, group: &Value) -> String {
    let mut line = format!("{label}: {}", money(&group["knownUsd"]));
    if group["limitUsd"].as_f64().is_some_and(|v| v != 0.0) {
        line.push_str(&format!(" of {}", money(&group["limitUsd"])));
    }
    if let Some(runs) = group["unknownRuns"].as_u64().filter(|v| *v > 0) {
        line.push_str(&format!(" ({runs} metered run{} without a cost yet)", if runs == 1 { "" } else { "s" }));
    }
    if group["warning"] == true {
        line.push_str(" · near the limit");
    }
    line
}
pub fn render_details(snapshot: &Value, sections: &Value, url: &str) -> String {
    let mut lines: Vec<String> = render::render_status(&serde_json::json!({"jiraKey":snapshot["jiraKey"],"jiraTitle":snapshot["jiraTitle"],"phase":snapshot["phase"],"status":snapshot["status"]}),url).lines().take(2).map(String::from).collect();
    for name in details::SECTIONS {
        let Some(v) = sections.get(name) else {
            continue;
        };
        match name {
            "requirements" => {
                lines.push(format!("\n*Requirements* ({})", s(v, "state")));
                let docs = v["documents"].as_array().unwrap();
                if docs.is_empty() {
                    lines.push("None written yet.".into());
                }
                for d in docs {
                    lines.push(format!("_{}_", escape(s(d, "title"))));
                    lines.push(q(s(d, "excerpt")));
                }
            }
            "findings" => {
                lines.push(format!("\n*Open findings* ({} blocking)", v["blocking"]));
                let open = v["open"].as_array().unwrap();
                if open.is_empty() {
                    lines.push("None open.".into());
                }
                for f in open {
                    let location = if s(f, "file").is_empty() {
                        String::new()
                    } else {
                        format!(
                            " · {}",
                            code(&format!(
                                "{}{}",
                                s(f, "file"),
                                f["line"].as_u64().map(|l| format!(":{l}")).unwrap_or_default()
                            ))
                        )
                    };
                    lines.push(format!(
                        "• [{}{}] {}{location}",
                        escape(s(f, "severity")),
                        if f["blocking"] == true { ", blocking" } else { "" },
                        escape(s(f, "title"))
                    ));
                }
                if v["more"].as_u64().unwrap_or(0) > 0 {
                    lines.push(format!("…and {} more", v["more"]));
                }
            }
            "errors" => {
                lines.push("\n*Errors*".into());
                let nodes = v["nodes"].as_array().unwrap();
                if s(v, "statusMessage").is_empty() && nodes.is_empty() {
                    lines.push("None.".into());
                }
                if !s(v, "statusMessage").is_empty() {
                    lines.push("Task:".into());
                    lines.push(q(s(v, "statusMessage")));
                }
                for n in nodes {
                    lines.push(format!("{}:", escape(s(n, "label"))));
                    lines.push(q(s(n, "error")));
                }
            }
            "cost" => {
                lines.push("\n*Cost*".into());
                lines.push(if v["total"].is_object() {
                    format!("Total: {}", usage(&v["total"]))
                } else {
                    "No runs yet.".into()
                });
                for n in v["nodes"].as_array().unwrap() {
                    lines.push(format!("• {}: {}", escape(s(n, "label")), usage(&n["usage"])));
                }
                if v["budget"].is_object() {
                    lines.push(spend("Ticket API spend", &v["budget"]["task"]));
                }
            }
            "activity" => {
                lines.push("\n*Recent activity*".into());
                let events = v["events"].as_array().unwrap();
                if events.is_empty() {
                    lines.push("Nothing yet.".into());
                }
                for e in events {
                    let when: String = s(e, "timestamp").chars().skip(5).take(11).collect();
                    lines.push(format!(
                        "• {} {}{}",
                        escape(&when.replace('T', " ")),
                        if matches!(s(e, "level"), "warning" | "error") { "⚠ " } else { "" },
                        escape(s(e, "message"))
                    ));
                }
            }
            "pr" => {
                lines.push("\n*Pull request*".into());
                if s(v, "url").is_empty() {
                    lines.push("None recorded yet.".into());
                } else {
                    lines.push(escape(s(v, "url")));
                    let dates: Vec<_> = [("created", "createdAt"), ("merged", "mergedAt")]
                        .iter()
                        .filter(|(_, k)| !s(v, k).is_empty())
                        .map(|(label, key)| {
                            format!("{label} {}", escape(&s(v, key).chars().take(10).collect::<String>()))
                        })
                        .collect();
                    if !dates.is_empty() {
                        let joined = dates.join(", ");
                        lines.push(format!("{}{}", joined[..1].to_uppercase(), joined[1..].to_lowercase()));
                    }
                }
                if v["syncPaused"] == true {
                    lines.push("GitHub PR sync is paused for this task.".into());
                }
            }
            "ticket" => {
                let when = s(v, "syncedAt").chars().take(16).collect::<String>().replace('T', " ");
                lines.push(format!(
                    "\n*Jira ticket*{}",
                    if when.is_empty() { String::new() } else { format!(" (synced {} UTC)", escape(&when)) }
                ));
                if v["synced"] != true {
                    lines.push("Not synced from Jira yet.".into());
                    continue;
                }
                lines.push(q(if s(v, "description").is_empty() { "(no description)" } else { s(v, "description") }));
                if v["comments"].is_null() {
                    lines.push("Comments were not synced.".into());
                } else if v["comments"]["latest"].as_array().unwrap().is_empty() {
                    lines.push("No comments.".into());
                } else {
                    let latest = v["comments"]["latest"].as_array().unwrap();
                    lines.push(format!("_Latest comments_ ({} of {})", latest.len(), v["comments"]["count"]));
                    for c in latest {
                        lines.push(format!("{}:", escape(s(c, "by"))));
                        lines.push(q(s(c, "excerpt")));
                    }
                }
            }
            _ => {}
        }
    }
    lines.push(format!("\n<{url}|Open in AgentFlow>"));
    lines.join("\n")
}
pub fn requirements(key: &str, documents: &[(String, String)]) -> Vec<String> {
    let count = documents.len();
    let mut messages = vec![format!(
        "*Requirements for {}*, in full: {count} document{}. The approve button follows the last one.",
        escape(key),
        if count == 1 { "" } else { "s" }
    )];
    for (title, text) in documents {
        messages.extend(render::chunks(
            &format!("*{}*\n{}", escape(title), escape(text.trim())),
            render::REQUIREMENTS_MESSAGE_LIMIT,
        ));
    }
    messages
}
pub fn ai_answer(text: &str) -> String {
    let body = regex::Regex::new(r"\*\*(.+?)\*\*").unwrap().replace_all(&escape(text), "*$1*").into_owned();
    format!("{body}\n_AI answer from AgentFlow's data; it can be wrong. Check AgentFlow before acting on it._")
}
pub fn digest(tasks: &[Value], cost: Option<&Value>) -> String {
    digest_at(tasks, cost, chrono::Utc::now())
}
pub fn digest_at(tasks: &[Value], cost: Option<&Value>, now: chrono::DateTime<chrono::Utc>) -> String {
    let mut live: Vec<_> =
        tasks.iter().filter(|t| !matches!(s(t, "status"), "MERGED" | "CANCELLED")).cloned().collect();
    let waiting = live.iter().filter(|t| t["attention"].as_array().is_some_and(|a| !a.is_empty())).count();
    let count = live.len();
    live.sort_by_key(|t| t["attention"].as_array().is_none_or(|a| a.is_empty()));
    let mut text = format!(
        "*AgentFlow digest*: {count} active task{}, {waiting} waiting on you",
        if count == 1 { "" } else { "s" }
    );
    for task in live.iter().take(25) {
        let mut line = format!("• {} · {}", render::heading(task), render::phase(task));
        if let Some(item) = task["attention"].as_array().and_then(|a| a.first()) {
            let age = chrono::DateTime::parse_from_rfc3339(s(item, "since"))
                .ok()
                .map(|at| {
                    let minutes = (now - at.with_timezone(&chrono::Utc)).num_seconds().div_euclid(60).max(0);
                    format!(
                        " for {}{}",
                        if minutes < 60 {
                            minutes
                        } else if minutes < 1440 {
                            minutes / 60
                        } else {
                            minutes / 1440
                        },
                        if minutes < 60 {
                            "m"
                        } else if minutes < 1440 {
                            "h"
                        } else {
                            "d"
                        }
                    )
                })
                .unwrap_or_default();
            line.push_str(&format!(" · needs you{age}: {}", render::needs_you(item)));
        }
        text.push_str(&format!("\n{line}"));
    }
    if count > 25 {
        text.push_str(&format!("\n…and {} more", count - 25));
    }
    if let Some(cost) = cost {
        text.push_str(&format!("\n{}", spend("Today's API spend", cost)));
    }
    text
}
