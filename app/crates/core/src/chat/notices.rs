//! Fixed labels only; provider diagnostics and findings are never included in notices.
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Notice {
    pub key: String,
    pub summary: String,
    pub question: Option<String>,
    pub choices: Vec<String>,
    pub question_uid: Option<String>,
}
pub fn notices(snapshot: &Value, notify: &[String], include_text: bool) -> IndexMap<String, Notice> {
    let mut found = IndexMap::new();
    let status = snapshot["status"].as_str().unwrap_or("");
    let summary = match status {
        "NEEDS_HUMAN_REVIEW" => {
            Some("Needs human review: the review loop stopped. Open AgentFlow to decide how to continue.")
        }
        "READY_FOR_PR" => Some("Ready for PR: final approval is recorded."),
        "FAILED" => Some("Task failed."),
        "CANCELLED" => Some("Task cancelled."),
        _ => None,
    };
    if notify.iter().any(|s| s == status)
        && let Some(summary) = summary
    {
        let key = format!("status:{status}");
        found.insert(key.clone(), Notice { key, summary: summary.into(), ..Default::default() });
    }
    // An open PR into a branch other than the one the task started from. Part of the PR
    // notifications, so it follows READY_FOR_PR; clears once the PR is retargeted.
    let target = snapshot["prBaseBranch"].as_str();
    if status == "PR_CREATED"
        && notify.iter().any(|s| s == "READY_FOR_PR")
        && target.is_some_and(|t| Some(t) != snapshot["baseBranch"].as_str())
    {
        let key = "pr:base-mismatch".to_string();
        let summary = "The PR targets a different branch than the task started from. Open AgentFlow to retarget it.";
        found.insert(key.clone(), Notice { key, summary: summary.into(), ..Default::default() });
    }
    for item in snapshot["attention"].as_array().into_iter().flatten() {
        let state = item["state"].as_str().unwrap_or("");
        let node = item["nodeKey"].as_str().unwrap_or("");
        let label = item["label"].as_str().filter(|s| !s.is_empty()).unwrap_or(node);
        if !notify.iter().any(|s| s == state) {
            continue;
        }
        let suffix = match state {
            "WAITING_FOR_USER" => "is waiting for your answer",
            "WAITING_FOR_APPROVAL" => "is waiting for your approval",
            "FAILED" => "failed",
            "INTERRUPTED" => "was interrupted; choose Resume or Stop & retry in AgentFlow",
            "READY" => "is ready to start",
            _ => continue,
        };
        let pending = snapshot["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|q| state == "WAITING_FOR_USER" && q["actionable"] == true && q["nodeKey"] == node)
            .collect::<Vec<_>>();
        if pending.is_empty() {
            let key = format!("node:{node}:{state}");
            found.insert(key.clone(), Notice { key, summary: format!("{label} {suffix}"), ..Default::default() });
            continue;
        }
        for q in pending {
            let uid = q["uid"].as_str().unwrap_or("");
            let key = format!("question:{uid}");
            found.insert(
                key.clone(),
                Notice {
                    key,
                    summary: format!("{label} asked a question"),
                    question: include_text.then(|| q["message"].as_str().map(String::from)).flatten(),
                    choices: if include_text {
                        q["choices"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c.as_str().map(String::from))
                            .collect()
                    } else {
                        vec![]
                    },
                    question_uid: Some(uid.into()),
                },
            );
        }
    }
    found
}
