use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn text(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: None,
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChatEvent {
    TextChunk(String),
    ToolCallDelta(ToolCallDelta),
    ToolCallDone(ToolCall),
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

#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub options: ChatOptions,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("api return error (status {status}): {message}")]
    Api { status: u16, message: String },

    #[error("failed to parse responses: {0}")]
    Parse(String),

    #[error("provider stream did not complete successfully: {0}")]
    StreamIncomplete(String),

    #[error("unsupported provider feature: {0}")]
    Unsupported(String),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ApiType {
    Responses,
    Completions,
}

impl FromStr for ApiType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "responses" => Ok(ApiType::Responses),
            "completions" => Ok(ApiType::Completions),
            _ => Err(format!("unknown api type: {s}")),
        }
    }
}

/// A boxed stream keeps provider implementations hidden behind the trait boundary.
/// Each item is a structured event so providers can stream text and tool activity.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatEvent, ProviderError>> + Send>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_effort_as_str_matches_expected_values() {
        assert_eq!(ReasoningEffort::Low.as_str(), "low");
        assert_eq!(ReasoningEffort::Medium.as_str(), "medium");
        assert_eq!(ReasoningEffort::High.as_str(), "high");
        assert_eq!(ReasoningEffort::XHigh.as_str(), "xhigh");
    }

    #[test]
    fn reasoning_effort_display_matches_as_str() {
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
        ] {
            assert_eq!(effort.to_string(), effort.as_str());
        }
    }

    #[test]
    fn reasoning_effort_from_str_accepts_supported_values() {
        assert_eq!(ReasoningEffort::from_str("low"), Ok(ReasoningEffort::Low));
        assert_eq!(
            ReasoningEffort::from_str("medium"),
            Ok(ReasoningEffort::Medium)
        );
        assert_eq!(ReasoningEffort::from_str("high"), Ok(ReasoningEffort::High));
        assert_eq!(
            ReasoningEffort::from_str("xhigh"),
            Ok(ReasoningEffort::XHigh)
        );
    }

    #[test]
    fn reasoning_effort_from_str_is_case_insensitive() {
        assert_eq!(ReasoningEffort::from_str("LOW"), Ok(ReasoningEffort::Low));
        assert_eq!(
            ReasoningEffort::from_str("Medium"),
            Ok(ReasoningEffort::Medium)
        );
        assert_eq!(
            ReasoningEffort::from_str("XHIGH"),
            Ok(ReasoningEffort::XHigh)
        );
    }

    #[test]
    fn reasoning_effort_from_str_rejects_unknown_value() {
        let err = ReasoningEffort::from_str("minimal").expect_err("value should be rejected");
        assert!(err.contains("minimal"));
    }

    #[test]
    fn reasoning_effort_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::High).unwrap(),
            "\"high\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::XHigh).unwrap(),
            "\"xhigh\""
        );
    }

    #[test]
    fn reasoning_effort_deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"medium\"").unwrap(),
            ReasoningEffort::Medium
        );
    }

    #[test]
    fn api_type_from_str_accepts_supported_values() {
        assert_eq!(ApiType::from_str("responses"), Ok(ApiType::Responses));
        assert_eq!(ApiType::from_str("completions"), Ok(ApiType::Completions));
    }

    #[test]
    fn api_type_from_str_is_case_insensitive() {
        assert_eq!(ApiType::from_str("RESPONSES"), Ok(ApiType::Responses));
        assert_eq!(ApiType::from_str("Completions"), Ok(ApiType::Completions));
    }

    #[test]
    fn api_type_from_str_rejects_unknown_value() {
        let err = ApiType::from_str("chat").expect_err("value should be rejected");
        assert!(err.contains("chat"));
    }
}
