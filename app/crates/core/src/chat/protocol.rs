//! Socket envelopes are acknowledged before slow operations; mutations require the operator DM.
use super::{actions, answers, forms, models::*, render, service::ChatService};
use serde_json::{Value, json};
use std::sync::Arc;
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    actions::s(v, k)
}
fn metadata(view: &Value) -> Value {
    serde_json::from_str(s(view, "private_metadata")).unwrap_or(Value::Null)
}
impl ChatService {
    pub(crate) async fn busy(&self, envelope: &Value) {
        let payload = &envelope["payload"];
        let response = match s(payload, "type") {
            "block_suggestion" => json!({"options":[]}),
            "view_submission" => {
                let field = match s(&payload["view"], "callback_id") {
                    "agentflow_task_form" => "ticket",
                    "agentflow_answer" => "answer",
                    _ => "text",
                };
                json!({"response_action":"errors","errors":{field:"AgentFlow is busy; try again shortly."}})
            }
            _ => json!({}),
        };
        let _ = self.ack(envelope, response).await;
    }
    pub(crate) async fn ack(&self, envelope: &Value, payload: Value) -> Result<(), ChatError> {
        let id = s(envelope, "envelope_id");
        let mut body = json!({"envelope_id":id});
        if payload.as_object().is_some_and(|o| !o.is_empty()) {
            body["payload"] = payload;
        }
        {
            let mut cache = self.envelopes.lock().unwrap();
            cache.insert(id.into(), Some(body.clone()));
            if cache.len() > 512 {
                cache.shift_remove_index(0);
            }
        }
        self.adapter.api("__ack", body).await?;
        Ok(())
    }
    pub(crate) async fn envelope(self: Arc<Self>, envelope: Value) {
        let id = s(&envelope, "envelope_id");
        if id.is_empty() {
            return;
        }
        let prior = {
            let mut cache = self.envelopes.lock().unwrap();
            if let Some(prior) = cache.get(id) {
                Some(prior.clone())
            } else {
                cache.insert(id.into(), None);
                None
            }
        };
        if let Some(prior) = prior {
            if let Some(body) = prior {
                let _ = self.adapter.api("__ack", body).await;
            }
            return;
        }
        let payload = &envelope["payload"];
        match s(&envelope, "type") {
            "interactive" => self.interactive(&envelope, payload).await,
            "events_api" => {
                if self.ack(&envelope, json!({})).await.is_err() {
                    return;
                }
                let event_id = s(payload, "event_id");
                if !event_id.is_empty() {
                    let mut cache = self.event_ids.lock().unwrap();
                    if cache.contains_key(event_id) {
                        return;
                    }
                    cache.insert(event_id.into(), ());
                    if cache.len() > 512 {
                        cache.shift_remove_index(0);
                    }
                }
                let event = &payload["event"];
                if s(event, "type") != "message"
                    || !s(event, "subtype").is_empty()
                    || !s(event, "bot_id").is_empty()
                    || !self.authorized(s(event, "user"))
                    || self.dm().as_deref() != Some(s(event, "channel"))
                {
                    return;
                }
                let request = MessageRequest {
                    user_id: s(event, "user").into(),
                    channel: s(event, "channel").into(),
                    thread_id: event["thread_ts"].as_str().map(str::to_owned),
                    text: render::unslack(s(event, "text")),
                };
                let reaction = json!({"channel":request.channel,"timestamp":event["ts"],"name":"eyes"});
                let seen = self.adapter.api("reactions.add", reaction.clone()).await.is_ok();
                let mut replied = false;
                if let Some(reply) = self.message(&request).await {
                    let mut body = reply_body(&request.channel, &reply);
                    if let Some(thread) = request.thread_id {
                        body["thread_ts"] = thread.into();
                    }
                    replied = self.adapter.api("chat.postMessage", body).await.is_ok();
                }
                if seen {
                    let _ = self.adapter.api("reactions.remove", reaction).await;
                    if replied {
                        let _ = self
                            .adapter
                            .api(
                                "reactions.add",
                                json!({"channel":request.channel,"timestamp":event["ts"],"name":"white_check_mark"}),
                            )
                            .await;
                    }
                }
            }
            "slash_commands" => {
                if self.ack(&envelope, json!({})).await.is_err()
                    || self.settings.command.as_deref() != Some(s(payload, "command"))
                {
                    return;
                }
                let text = render::unslack(s(payload, "text"));
                let words: Vec<_> = text.split_whitespace().collect();
                let reply = if words.first().is_some_and(|w| w.eq_ignore_ascii_case("new")) {
                    self.open_task(s(payload, "user_id"), s(payload, "trigger_id"), words.get(1).copied())
                        .await
                        .err()
                        .map(Reply::text)
                } else {
                    Some(self.command(s(payload, "user_id"), &text).await)
                };
                if let Some(reply) = reply {
                    let mut body = reply_body(s(payload, "channel_id"), &reply);
                    body["user"] = payload["user_id"].clone();
                    let _ = self.adapter.api("chat.postEphemeral", body).await;
                }
            }
            _ => {
                let _ = self.ack(&envelope, json!({})).await;
            }
        }
    }
    async fn open_task(&self, user: &str, trigger: &str, key: Option<&str>) -> Result<(), String> {
        let form = self.new_task_form(user, key)?;
        self.adapter
            .api("views.open", json!({"trigger_id":trigger,"view":forms::task_modal(&form)}))
            .await
            .map_err(|_| "Could not open the task form; try again.".to_owned())?;
        Ok(())
    }
    async fn interactive(self: &Arc<Self>, envelope: &Value, payload: &Value) {
        let user = s(&payload["user"], "id");
        match s(payload, "type") {
            "block_suggestion" => {
                let field = s(payload, "action_id").strip_prefix("agentflow_task_").unwrap_or("");
                let repository = forms::selected(&payload["view"]["state"]["values"], "repository");
                let options = self
                    .options(&OptionsRequest {
                        user_id: user.into(),
                        field: field.into(),
                        query: s(payload, "value").into(),
                        repository: (!repository.is_empty()).then(|| repository.into()),
                    })
                    .await;
                let options: Vec<_> = options.iter().filter_map(|(v, l)| render::option(v, l)).collect();
                let _ = self.ack(envelope, json!({"options":options})).await;
            }
            "view_submission" => {
                let view = &payload["view"];
                let values = &view["state"]["values"];
                let errors = match s(view, "callback_id") {
                    "agentflow_task_form" => self.submit_task(forms::task_draft(user, values)),
                    "agentflow_text_form" => {
                        let meta = metadata(view);
                        self.submit_text(TextSubmission {
                            user_id: user.into(),
                            kind: s(&meta, "kind").into(),
                            value: s(&meta, "value").into(),
                            text: forms::selected(values, "text").into(),
                        })
                    }
                    "agentflow_answer" => {
                        if self.ack(envelope, json!({})).await.is_err() {
                            return;
                        }
                        let meta = metadata(view);
                        let request = AnswerRequest {
                            user_id: user.into(),
                            channel: s(&meta, "channel").into(),
                            jira_key: s(&meta, "jira_key").into(),
                            question_uid: s(&meta, "question_uid").into(),
                            text: Some(forms::selected(values, "answer").into()),
                            ..Default::default()
                        };
                        self.deliver_answer(&request).await;
                        return;
                    }
                    _ => json!({}),
                };
                let _ = self
                    .ack(
                        envelope,
                        if errors.as_object().is_none_or(|o| o.is_empty()) {
                            json!({})
                        } else {
                            json!({"response_action":"errors","errors":errors})
                        },
                    )
                    .await;
            }
            "block_actions" => {
                if self.ack(envelope, json!({})).await.is_err()
                    || !self.authorized(user)
                    || self.dm().as_deref() != Some(s(&payload["channel"], "id"))
                {
                    return;
                }
                let Some(action) = payload["actions"].as_array().and_then(|a| a.first()) else {
                    return;
                };
                let id = s(action, "action_id");
                let value = s(action, "value");
                let channel = s(&payload["channel"], "id");
                if id == "agentflow_answer_reply" {
                    let Some((key, uid, "reply")) = render::parse_reference(value) else {
                        return;
                    };
                    if !self.settings.answering() {
                        return;
                    }
                    let blocks: Vec<_> = payload["message"]["blocks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|b| b["type"] == "section")
                        .take(10)
                        .cloned()
                        .collect();
                    let meta = json!({"jira_key":key,"question_uid":uid,"channel":channel,"thread_id":payload["message"]["thread_ts"]});
                    let _ = self
                        .adapter
                        .api(
                            "views.open",
                            json!({"trigger_id":payload["trigger_id"],"view":forms::answer_modal(&blocks,&meta)}),
                        )
                        .await;
                    return;
                }
                let numbered = |prefix: &str| {
                    id.strip_prefix(prefix)
                        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
                };
                if numbered("agentflow_answer_follow_") || id == "agentflow_answer_back" {
                    // A decision tree's follow-up option, or "Change branch": the path rides in the value.
                    let Some((key, uid, choice)) = render::parse_reference(value) else {
                        return;
                    };
                    let path = render::parse_path(choice);
                    if path.is_none() && choice != "back" {
                        return;
                    }
                    let request = AnswerRequest {
                        user_id: user.into(),
                        channel: channel.into(),
                        jira_key: key.into(),
                        question_uid: uid.into(),
                        path,
                        back: path.is_none(),
                        ..Default::default()
                    };
                    self.deliver_choice(&request, &payload["message"]).await;
                    return;
                }
                if id == "agentflow_answer_typed" || numbered("agentflow_answer_choice_") {
                    let Some((key, uid, text)) = (if id == "agentflow_answer_typed" {
                        render::parse_offer(value)
                    } else {
                        render::parse_reference(value)
                    }) else {
                        return;
                    };
                    let index = if id == "agentflow_answer_typed" {
                        None
                    } else {
                        let Ok(index) = text.parse::<usize>() else {
                            return;
                        };
                        Some(index)
                    };
                    let request = AnswerRequest {
                        user_id: user.into(),
                        channel: channel.into(),
                        jira_key: key.into(),
                        question_uid: uid.into(),
                        choice_index: index,
                        text: index.is_none().then(|| text.into()),
                        ..Default::default()
                    };
                    if index.is_some() {
                        self.deliver_choice(&request, &payload["message"]).await;
                    } else if self.deliver_answer(&request).await {
                        let _ = self
                            .adapter
                            .update(
                                channel,
                                s(&payload["message"], "ts"),
                                &format!(
                                    "*Sent as your answer.*\n> {}",
                                    render::escape(request.text.as_deref().unwrap_or("").trim())
                                ),
                            )
                            .await;
                    }
                    return;
                }
                let Some((base, index)) = id.rsplit_once('_') else {
                    return;
                };
                if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
                    return;
                }
                let Some(kind) = base.strip_prefix("agentflow_").filter(|k| {
                    [
                        "new_task",
                        "run",
                        "request_changes",
                        "mark_pr",
                        "cancel",
                        "mark_merged",
                        "review_requirements",
                        "approve_requirements",
                    ]
                    .contains(k)
                }) else {
                    return;
                };
                let reason = if kind == "new_task" {
                    self.open_task(user, s(payload, "trigger_id"), Some(value)).await.err()
                } else if ["request_changes", "mark_pr"].contains(&kind) {
                    match self.text_form(user, kind, value) {
                        Ok(form) => self
                            .adapter
                            .api(
                                "views.open",
                                json!({"trigger_id":payload["trigger_id"],"view":forms::text_modal(&form)}),
                            )
                            .await
                            .err()
                            .map(|_| "Could not open the form; try again.".into()),
                        Err(reason) => Some(reason),
                    }
                } else {
                    self.action(&ActionRequest {
                        user_id: user.into(),
                        channel: channel.into(),
                        kind: kind.into(),
                        value: value.into(),
                    })
                    .await
                };
                if let Some(reason) = reason {
                    let _ = self
                        .post(Some(value.split('|').next().unwrap_or(value)), Reply::text(render::escape(&reason)))
                        .await;
                } else if let Some(done) = match kind {
                    "run" => Some(format!(
                        "*{} started.*",
                        render::escape(s(&action["text"], "text").strip_prefix("Run ").unwrap_or("Stage"))
                    )),
                    "cancel" => Some("*Task cancelled.*".into()),
                    "mark_merged" => Some("*Marked merged.*".into()),
                    "approve_requirements" => Some("*Requirements approved.*".into()),
                    _ => None,
                } {
                    let mut sections: Vec<_> = payload["message"]["blocks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|b| b["type"] == "section")
                        .filter_map(|b| b["text"]["text"].as_str())
                        .collect();
                    sections.push(&done);
                    let _ = self.adapter.update(channel, s(&payload["message"], "ts"), &sections.join("\n")).await;
                }
            }
            _ => {
                let _ = self.ack(envelope, json!({})).await;
            }
        }
    }
    async fn deliver_answer(&self, request: &AnswerRequest) -> bool {
        let reason = answers::answer(&self.engine, &self.settings, self.dm().as_deref(), request).await;
        if let Some(reason) = reason {
            self.refuse(request, &reason).await;
            false
        } else {
            true
        }
    }
    /// An answer control on a notice: delivered, refused, or a decision tree's branch (or "Change
    /// branch") that swaps the controls of `message`, the notice clicked, in place.
    async fn deliver_choice(&self, request: &AnswerRequest, message: &Value) {
        let reason = match answers::respond(&self.engine, &self.settings, self.dm().as_deref(), request).await {
            Ok(None) => return,
            Ok(Some(prompt)) => match self.show_prompt(&request.channel, message, &prompt).await {
                None => return,
                Some(reason) => reason,
            },
            Err(reason) => reason,
        };
        self.refuse(request, &reason).await;
    }
    async fn refuse(&self, request: &AnswerRequest, reason: &str) {
        if self.authorized(&request.user_id) {
            let _ = self.post(Some(&request.jira_key), Reply::text(render::escape(reason))).await;
        }
    }
    /// Edit the notice in place: its question text, then `prompt`'s controls. None when shown.
    async fn show_prompt(&self, channel: &str, message: &Value, prompt: &AnswerPrompt) -> Option<String> {
        let ts = s(message, "ts");
        if ts.is_empty() {
            return Some("Not shown: answer it in AgentFlow.".into());
        }
        let mut blocks: Vec<Value> = message["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "section" && b["block_id"] != render::FOLLOW_UP_BLOCK)
            .cloned()
            .collect();
        blocks.extend(render::answer_blocks(prompt));
        let text = Some(s(message, "text")).filter(|t| !t.is_empty()).unwrap_or("AgentFlow question");
        let body = json!({"channel":channel,"ts":ts,"text":text,"blocks":blocks});
        match self.adapter.api("chat.update", body).await {
            Ok(_) => None,
            Err(_) => Some("Not shown: Slack refused the follow-up. Answer it in AgentFlow.".into()),
        }
    }
}
pub(crate) fn reply_body(channel: &str, reply: &Reply) -> Value {
    let mut body = json!({"channel":channel,"text":reply.text,"unfurl_links":false,"unfurl_media":false});
    if let Some(blocks) = render::reply_blocks(reply) {
        body["blocks"] = blocks.into();
    }
    body
}
