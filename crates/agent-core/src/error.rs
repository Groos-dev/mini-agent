#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("provider error: {0}")]
    Provider(#[from] provider::ProviderError),

    #[error("tool error: {0}")]
    Tool(#[from] crate::tool::ToolError),

    #[error("invalid tool arguments for {tool_name}: {message}")]
    InvalidToolArguments { tool_name: String, message: String },

    #[error("tool-call round limit of {limit} exceeded")]
    ToolCallLimitExceeded { limit: usize },
}
