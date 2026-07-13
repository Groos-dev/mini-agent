#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("model error: {0}")]
    Model(#[from] agent_protocol::ModelError),

    #[error("tool error: {0}")]
    Tool(#[from] crate::tool::ToolError),

    #[error("invalid tool arguments for {tool_name}: {message}")]
    InvalidToolArguments { tool_name: String, message: String },
}
