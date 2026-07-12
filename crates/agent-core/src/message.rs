use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        content: Option<String>,
        tool_calls: Vec<provider::ToolCall>,
    },
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

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: Some(content.into()),
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant_response(
        content: Option<String>,
        tool_calls: Vec<provider::ToolCall>,
    ) -> Self {
        Self::Assistant {
            content,
            tool_calls,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_variants_keep_role_specific_fields_together() {
        assert!(matches!(Message::user("hello"), Message::User(content) if content == "hello"));
        assert!(matches!(
            Message::tool_result("call_1", "result"),
            Message::ToolResult { tool_call_id, content }
                if tool_call_id == "call_1" && content == "result"
        ));
    }

    #[test]
    fn assistant_response_preserves_text_and_tool_calls() {
        let calls = vec![provider::ToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
        }];

        let message = Message::assistant_response(Some("checking".to_string()), calls.clone());

        assert!(matches!(
            message,
            Message::Assistant { content: Some(content), tool_calls }
                if content == "checking" && tool_calls[0].id == calls[0].id
        ));
    }
}
