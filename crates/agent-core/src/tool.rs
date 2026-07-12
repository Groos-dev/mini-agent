use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use provider::ToolSpec;
use serde::{Deserialize, Serialize};

pub mod shell;

pub type ToolApproval = Arc<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>;

#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

impl Default for ToolExecutionContext {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            timeout: Duration::from_secs(30),
            max_stdout_bytes: 64 * 1024,
            max_stderr_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub content: String,
    pub metadata: serde_json::Value,
}

impl ToolResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            success: true,
            content: content.into(),
            metadata: serde_json::Value::Null,
        }
    }

    pub fn failure(content: impl Into<String>) -> Self {
        Self {
            success: false,
            content: content.into(),
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    #[error("invalid tool input: {0}")]
    InvalidInput(String),

    #[error("tool execution denied: {0}")]
    Denied(String),

    #[error("tool timed out after {0:?}")]
    Timeout(Duration),

    #[error("tool execution failed: {0}")]
    Execution(String),
}

#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    fn spec(&self) -> ToolSpec;

    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> Result<ToolResult, ToolError>;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn ToolExecutor>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn ToolExecutor>) -> Result<(), ToolError> {
        let spec = tool.spec();
        if self.tools.contains_key(&spec.name) {
            return Err(ToolError::InvalidInput(format!(
                "duplicate tool name: {}",
                spec.name
            )));
        }
        self.tools.insert(spec.name, tool);
        Ok(())
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        input: serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::UnknownTool(name.to_string()))?;
        tool.execute(input, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use provider::ToolSpec;

    use super::*;

    struct TestTool;

    #[async_trait::async_trait]
    impl ToolExecutor for TestTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "test".to_string(),
                description: "test tool".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: ToolExecutionContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::success("ok"))
        }
    }

    #[tokio::test]
    async fn registry_rejects_duplicate_names_and_reports_unknown_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TestTool)).unwrap();

        let duplicate = registry.register(Arc::new(TestTool)).unwrap_err();
        assert!(matches!(duplicate, ToolError::InvalidInput(_)));

        let missing = registry
            .execute(
                "missing",
                serde_json::json!({}),
                ToolExecutionContext::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(missing, ToolError::UnknownTool(name) if name == "missing"));
    }
}
