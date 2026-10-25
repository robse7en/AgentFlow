//! Only explicitly allowed sections are read. Used by both chat views and model context.
use serde_json::{Value, json};
pub const SECTIONS: [&str; 7] = ["requirements", "findings", "errors", "cost", "activity", "pr", "ticket"];
pub const TITLES: [(&str, &str); 5] = [
    ("requirements", "Requirements"),
    ("acceptanceCriteria", "Acceptance criteria"),
    ("assumptions", "Assumptions"),
    ("implementationPlan", "Implementation plan"),
    ("risks", "Risks"),
];
pub fn title(name: &str) -> &str {
    TITLES.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap_or(name)
}
pub fn trim(text: &str, chars: usize, lines: Option<usize>) -> String {
    let kept: Vec<_> =
        crate::engine::prompts::py_splitlines(text.trim()).into_iter().filter(|s| !s.trim().is_empty()).collect();
    let mut cut = lines.is_some_and(|n| kept.len() > n);
    let joined = kept.iter().take(lines.unwrap_or(kept.len())).copied().collect::<Vec<_>>().join("\n");
    let mut text = joined.clone();
    if joined.chars().count() > chars {
        text = joined.chars().take(chars).collect::<String>().trim_end().into();
        cut = true;
    }
    if cut {
        text.push('…');
    }
    text
}
fn array<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn project(value: &Value, keys: &[&str]) -> Value {
    keys.iter().map(|&k| (k.into(), value[k].clone())).collect::<serde_json::Map<_, _>>().into()
}
pub fn task_details(snapshot: &Value, allowed: &[String], wanted: Option<&[String]>, activity_limit: usize) -> Value {
    let mut result = serde_json::Map::new();
    for name in SECTIONS {
        if !allowed.iter().any(|s| s == name) || wanted.is_some_and(|w| !w.iter().any(|s| s == name)) {
            continue;
        }
        let value = match name {
            "requirements" => {
                let docs: Vec<_> = TITLES
                    .iter()
                    .filter_map(|(key, title)| {
                        let text = string(&snapshot["requirements"], key);
                        (!text.trim().is_empty()).then(|| json!({"title":title,"excerpt":trim(text,600,Some(6))}))
                    })
                    .collect();
                let state = if snapshot["requirementsDigest"].is_null() {
                    "draft"
                } else if snapshot["requirementsDigest"] == snapshot["requirementsDraftDigest"] {
                    "approved"
                } else {
                    "changed since approval"
                };
                json!({"documents":docs,"state":state})
            }
            "findings" => findings(snapshot),
            "errors" => {
                let nodes: Vec<_> = array(snapshot,"nodes").filter(|n| !string(n,"error").is_empty()).map(|n| json!({"label":n.get("label").filter(|v| v.as_str().is_some_and(|s| !s.is_empty())).unwrap_or(&n["key"]),"error":trim(string(n,"error"),500,Some(6))})).collect();
                let message = string(snapshot, "statusMessage");
                json!({"statusMessage":(!message.is_empty()).then(||trim(message,500,Some(6))),"nodes":nodes})
            }
            "cost" => {
                let nodes: Vec<_> = array(snapshot, "nodes")
                    .filter(|n| n["usage"].is_object())
                    .map(|n| json!({"label":n.get("label").filter(|v| v.as_str().is_some_and(|s| !s.is_empty())).unwrap_or(&n["key"]),"usage":n["usage"]}))
                    .collect();
                json!({"total":snapshot["totalUsage"],"nodes":nodes,"budget":snapshot["budget"]})
            }
            "activity" => {
                let events: Vec<_> = array(snapshot, "events").collect();
                json!({"events":events.iter().skip(events.len().saturating_sub(activity_limit.clamp(1,30))).map(|e| json!({"timestamp":e["timestamp"],"level":e["level"],"message":trim(string(e,"message"),300,None)})).collect::<Vec<_>>()})
            }
            "pr" => {
                json!({"url":snapshot["prUrl"],"createdAt":snapshot["prCreatedAt"],"mergedAt":snapshot["mergedAt"],"syncPaused":snapshot["prSyncPaused"] == true})
            }
            "ticket" => ticket(&snapshot["jiraIssue"]),
            _ => unreachable!(),
        };
        result.insert(name.into(), value);
    }
    result.into()
}
fn findings(snapshot: &Value) -> Value {
    let mut open: Vec<_> = array(snapshot, "findings").filter(|f| f["status"] == "open").collect();
    open.sort_by_key(|f| {
        (
            f["blocking"] != true,
            match string(f, "severity") {
                "critical" => 0,
                "high" => 1,
                "medium" => 2,
                "low" => 3,
                "info" => 4,
                _ => 5,
            },
        )
    });
    let shown: Vec<_> = open
        .iter()
        .take(15)
        .map(|f| {
            let mut v = project(f, &["severity", "file", "line", "type"]);
            v["blocking"] = (f["blocking"] == true).into();
            v["title"] = trim(string(f, "title"), 200, None).into();
            v
        })
        .collect();
    json!({"open":shown,"more":open.len().saturating_sub(15),"blocking":open.iter().filter(|f| f["blocking"] == true).count()})
}
fn ticket(issue: &Value) -> Value {
    if issue.as_object().is_none_or(|o| o.is_empty()) {
        return json!({"synced":false});
    }
    let markdown = string(issue, "markdown");
    let body =
        if markdown.starts_with("# ") { markdown.split_once('\n').map(|(_, s)| s).unwrap_or("") } else { markdown };
    let description = regex::Regex::new(r"\A\s*## Description[ \t]*\n").unwrap().replace(body, "");
    let headings = regex::Regex::new(r"(?m)^## Comments(?: \((\d+)\))?[ \t]*$").unwrap();
    let mut comments = Value::Null;
    let text = if let Some(heading) = headings.captures(&description) {
        if let Some(count) = heading.get(1) {
            let pattern = regex::Regex::new(r"(?m)^### Comment \S+ - (.+?)[ \t]*$").unwrap();
            let rest = &description[heading.get(0).unwrap().end()..];
            let matches: Vec<_> = pattern.captures_iter(rest).collect();
            let latest: Vec<_> = matches.iter().enumerate().skip(matches.len().saturating_sub(3)).map(|(i,m)| {
                let end = matches.get(i+1).map(|m| m.get(0).unwrap().start()).unwrap_or(rest.len());
                json!({"by":trim(m.get(1).unwrap().as_str(),100,None),"excerpt":trim(&rest[m.get(0).unwrap().end()..end],300,Some(6))})
            }).collect();
            comments = json!({"count":count.as_str().parse::<usize>().unwrap_or(0),"latest":latest});
        }
        &description[..heading.get(0).unwrap().start()]
    } else {
        &description
    };
    json!({"synced":true,"syncedAt":issue["syncedAt"],"description":trim(text,1000,Some(15)),"comments":comments})
}
