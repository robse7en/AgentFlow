use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ChatError {
    pub message: String,
    pub permanent: bool,
}
impl ChatError {
    pub fn temporary(message: impl Into<String>) -> Self {
        Self { message: message.into(), permanent: false }
    }
    pub fn permanent(message: impl Into<String>) -> Self {
        Self { message: message.into(), permanent: true }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Button {
    pub kind: String,
    pub label: String,
    pub value: String,
    pub confirm: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnswerPrompt {
    pub jira_key: String,
    pub question_uid: String,
    pub choices: Vec<String>,
    /// A decision tree (engine `decisions`), only when question text may be posted. With
    /// `branch`, the controls are that branch's follow-up options instead of the top-level choices.
    #[serde(default)]
    pub decision: Option<Value>,
    #[serde(default)]
    pub branch: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnswerOffer {
    pub jira_key: String,
    pub question_uid: String,
    pub text: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Reply {
    pub text: String,
    pub offer: Option<AnswerOffer>,
    #[serde(default)]
    pub buttons: Vec<Button>,
}
impl Reply {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub channel: String,
    pub text: String,
    pub thread_id: Option<String>,
    pub prompt: Option<AnswerPrompt>,
    pub buttons: Vec<Button>,
}
/// Someone used an answer control: a choice (`choice_index`), a follow-up option (`path`: branch
/// and option), "Change branch" (`back`), or free text (`text`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnswerRequest {
    pub user_id: String,
    pub channel: String,
    pub jira_key: String,
    pub question_uid: String,
    pub choice_index: Option<usize>,
    pub text: Option<String>,
    #[serde(default)]
    pub path: Option<(usize, usize)>,
    #[serde(default)]
    pub back: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageRequest {
    pub user_id: String,
    pub channel: String,
    pub thread_id: Option<String>,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionRequest {
    pub user_id: String,
    pub channel: String,
    pub kind: String,
    pub value: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskForm {
    pub jira_key: Option<String>,
    pub repository: Option<String>,
    pub search_tickets: bool,
    pub implementation_mode: String,
    pub base_branch: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OptionsRequest {
    pub user_id: String,
    pub field: String,
    pub query: String,
    pub repository: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskDraft {
    pub user_id: String,
    pub jira_key: String,
    pub repository_path: String,
    pub base_branch: String,
    pub project_relative_path: Option<String>,
    pub implementation_mode: Option<String>,
    pub additional_context: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextForm {
    pub kind: String,
    pub value: String,
    pub title: String,
    pub intro: String,
    pub label: String,
    pub multiline: bool,
    pub max_length: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextSubmission {
    pub user_id: String,
    pub kind: String,
    pub value: String,
    pub text: String,
}

/// Production Socket Mode and recording test transports share this contract.
#[async_trait::async_trait]
pub trait ChatAdapter: Send + Sync {
    async fn connect(&self) -> Result<mpsc::Receiver<Value>, ChatError>;
    async fn disconnect(&self);
    async fn open_direct(&self, user_id: &str) -> Result<String, ChatError>;
    async fn send(&self, message: ChatMessage) -> Result<String, ChatError>;
    async fn update(&self, channel: &str, id: &str, text: &str) -> Result<(), ChatError>;
    /// Socket-mode responses and modal operations; recorded by fake adapters.
    async fn api(&self, method: &str, body: Value) -> Result<Value, ChatError>;
}
