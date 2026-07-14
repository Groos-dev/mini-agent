use futures::Stream;
use serde::{Deserialize, Serialize};
use std::{pin::Pin, str::FromStr};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    System(String),
    User(String),
    AssistantText(String),
    ToolCall(ToolCall),
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System(content.into())
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User(content.into())
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self::AssistantText(content.into())
    }

    pub fn tool_call(tool_call: ToolCall) -> Self {
        Self::ToolCall(tool_call)
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelEvent {
    AssistantTextDelta(String),
    AssistantMessageDone(String),
    ToolCallDelta(ToolCallDelta),
    ToolCallRequestReady(ToolCall),
    ResponseCompleted,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    XHigh,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            _ => Err(format!("unknown reasoning effort: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ModelOptions {
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub options: ModelOptions,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model transport failed: {0}")]
    Transport(String),

    #[error("model API returned {status}: {message}")]
    Api { status: u16, message: String },

    #[error("failed to parse model response: {0}")]
    Parse(String),

    #[error("model stream did not complete successfully: {0}")]
    StreamIncomplete(String),

    #[error("unsupported model feature: {0}")]
    Unsupported(String),
}

pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelEvent, ModelError>> + Send>>;

#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_keep_tool_calls_and_results_as_distinct_items() {
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
        };

        assert!(matches!(
            Message::tool_call(call.clone()),
            Message::ToolCall(tool_call) if tool_call.id == call.id
        ));
        assert!(matches!(
            Message::tool_result("call_1", "ok"),
            Message::ToolResult { tool_call_id, content }
                if tool_call_id == "call_1" && content == "ok"
        ));
    }

    #[test]
    fn system_message_constructor_preserves_content() {
        assert!(matches!(
            Message::system("instructions"),
            Message::System(content) if content == "instructions"
        ));
    }

    #[test]
    fn user_message_constructor_preserves_content() {
        assert!(matches!(
            Message::user("question"),
            Message::User(content) if content == "question"
        ));
    }

    #[test]
    fn assistant_message_constructor_preserves_content() {
        assert!(matches!(
            Message::assistant_text("answer"),
            Message::AssistantText(content) if content == "answer"
        ));
    }

    #[test]
    fn reasoning_effort_parses_low() {
        assert_eq!("low".parse(), Ok(ReasoningEffort::Low));
    }

    #[test]
    fn reasoning_effort_parses_medium() {
        assert_eq!("medium".parse(), Ok(ReasoningEffort::Medium));
    }

    #[test]
    fn reasoning_effort_parses_high() {
        assert_eq!("high".parse(), Ok(ReasoningEffort::High));
    }

    #[test]
    fn reasoning_effort_parses_xhigh() {
        let effort: ReasoningEffort = "xhigh".parse().unwrap();
        assert_eq!(effort.as_str(), "xhigh");
        assert_eq!(effort.to_string(), "xhigh");
    }

    #[test]
    fn reasoning_effort_rejects_unknown_values() {
        assert_eq!(
            "maximum".parse::<ReasoningEffort>(),
            Err("unknown reasoning effort: maximum".to_string())
        );
    }
}
