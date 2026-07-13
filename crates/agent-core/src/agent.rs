use agent_protocol::{
    Message, ModelEvent, ModelOptions, ModelProvider, ModelRequest, ModelStream, ToolCall,
};
use async_stream::stream;
use futures::StreamExt;
use std::pin::Pin;
use tracing::{debug, info, warn};

use crate::{
    error::CoreError,
    tool::{ToolExecutionContext, ToolRegistry, ToolResult},
};

pub type AgentEventStream<'a> =
    Pin<Box<dyn futures::Stream<Item = Result<AgentEvent, CoreError>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextChunk(String),
    ToolCallStarted {
        id: String,
        name: String,
        arguments: String,
    },
    ToolCallFinished {
        id: String,
        name: String,
        result: ToolResult,
    },
    ToolCallFailed {
        id: String,
        name: String,
        error: String,
    },
    TurnFinished,
}

#[derive(Clone)]
pub struct AgentRunOptions {
    pub model_options: ModelOptions,
    pub tool_context: ToolExecutionContext,
}

impl AgentRunOptions {
    pub fn from_model_options(model_options: ModelOptions) -> Self {
        Self {
            model_options,
            tool_context: ToolExecutionContext::default(),
        }
    }
}

pub struct Agent {
    provider: Box<dyn ModelProvider>,
    history: Vec<Message>,
    tool_registry: ToolRegistry,
}

impl Agent {
    pub fn new(provider: Box<dyn ModelProvider>, tool_registry: ToolRegistry) -> Self {
        Self {
            provider,
            history: Vec::new(),
            tool_registry,
        }
    }

    pub async fn run_stream(
        &mut self,
        user_input: impl Into<String>,
        options: AgentRunOptions,
    ) -> Result<AgentEventStream<'_>, CoreError> {
        let user_input = user_input.into();
        self.history.push(Message::user(user_input));

        let s = stream! {
            loop {
                debug!(
                    history_messages = self.history.len(),
                    "requesting model turn"
                );
                let mut provider_stream = match self.run_turn_once(&self.history, &options).await {
                    Ok(stream) => stream,
                    Err(err) => {
                        warn!(error = %err, "failed to start model stream");
                        yield Err(err);
                        return;
                    }
                };

                let mut tool_calls = Vec::new();
                while let Some(event) = provider_stream.next().await {
                    match event {
                        Ok(ModelEvent::AssistantTextDelta(text)) => {
                            yield Ok(AgentEvent::TextChunk(text));
                        }
                        Ok(ModelEvent::AssistantMessageDone(content)) => {
                            self.history.push(Message::assistant_text(content));
                        }
                        Ok(ModelEvent::ToolCallRequestReady(tool_call)) => {
                            self.history
                                .push(Message::tool_call(tool_call.clone()));
                            tool_calls.push(tool_call);
                        }
                        Ok(ModelEvent::ToolCallDelta(_)) => {
                            // TODO: 如果需要实时展示 tool call 参数增量，可在这里转换为 AgentEvent。
                        }
                        Ok(ModelEvent::ResponseCompleted) => {}
                        Err(err) => {
                            warn!(error = %err, "model stream failed");
                            yield Err(CoreError::Model(err));
                            return;
                        }
                    }
                }

                if tool_calls.is_empty() {
                    let history_messages = self.history.len();
                    debug!(history_messages, "agent turn completed");
                    yield Ok(AgentEvent::TurnFinished);
                    return;
                }

                info!(
                    tool_call_count = tool_calls.len(),
                    "received tool calls"
                );

                for tool_call in tool_calls {
                    info!(
                        tool_name = %tool_call.name,
                        tool_call_id = %tool_call.id,
                        "executing tool call"
                    );
                    yield Ok(AgentEvent::ToolCallStarted {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                    });

                    match self.execute_tool_call(&tool_call, &options).await {
                        Ok(result) => {
                            info!(
                                tool_name = %tool_call.name,
                                tool_call_id = %tool_call.id,
                                success = result.success,
                                "tool call completed"
                            );
                            self.history.push(Message::tool_result(
                                tool_call.id.clone(),
                                self.format_tool_result(&result),
                            ));
                            yield Ok(AgentEvent::ToolCallFinished {
                                id: tool_call.id,
                                name: tool_call.name,
                                result,
                            });
                        }
                        Err(err) => {
                            let error = err.to_string();
                            warn!(
                                tool_name = %tool_call.name,
                                tool_call_id = %tool_call.id,
                                error = %error,
                                "tool call failed"
                            );
                            self.history.push(Message::tool_result(
                                tool_call.id.clone(),
                                self.format_tool_error(&error),
                            ));
                            yield Ok(AgentEvent::ToolCallFailed {
                                id: tool_call.id,
                                name: tool_call.name,
                                error,
                            });
                        }
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }

    async fn run_turn_once(
        &self,
        messages: &[Message],
        options: &AgentRunOptions,
    ) -> Result<ModelStream, CoreError> {
        let request = ModelRequest {
            messages: messages.to_vec(),
            tools: self.tool_registry.specs(),
            options: options.model_options,
        };
        Ok(self.provider.stream(request).await?)
    }

    async fn execute_tool_call(
        &self,
        tool_call: &ToolCall,
        options: &AgentRunOptions,
    ) -> Result<ToolResult, CoreError> {
        let input = serde_json::from_str(&tool_call.arguments).map_err(|err| {
            CoreError::InvalidToolArguments {
                tool_name: tool_call.name.clone(),
                message: err.to_string(),
            }
        })?;
        Ok(self
            .tool_registry
            .execute(&tool_call.name, input, &options.tool_context)
            .await?)
    }

    fn format_tool_result(&self, result: &ToolResult) -> String {
        serde_json::to_string(result).unwrap_or_else(|_| result.content.clone())
    }

    fn format_tool_error(&self, error: &str) -> String {
        serde_json::json!({
            "success": false,
            "error": error,
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use agent_protocol::{ModelError, ToolSpec};
    use async_trait::async_trait;
    use futures::{StreamExt, stream};

    use super::*;
    use crate::tool::{ToolError, ToolExecutor};

    type FakeEvents = Vec<Result<ModelEvent, ModelError>>;
    type FakeResponseQueue = VecDeque<FakeEvents>;

    #[derive(Clone)]
    struct FakeProvider {
        requests: Arc<Mutex<Vec<ModelRequest>>>,
        responses: Arc<Mutex<FakeResponseQueue>>,
    }

    impl FakeProvider {
        fn new(responses: Vec<FakeEvents>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(responses.into())),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for FakeProvider {
        async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
            self.requests.lock().unwrap().push(request);
            let events = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected provider request");
            Ok(Box::pin(stream::iter(events)))
        }
    }

    struct EchoTool;

    #[async_trait]
    impl ToolExecutor for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "echo".to_string(),
                description: "Returns its input".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            input: serde_json::Value,
            _ctx: &ToolExecutionContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::success(input.to_string()))
        }
    }

    fn tool_call() -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            arguments: r#"{"value":"one"}"#.to_string(),
        }
    }

    fn agent_with_responses(responses: Vec<FakeEvents>) -> (Agent, FakeProvider) {
        let provider = FakeProvider::new(responses);
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(EchoTool)).unwrap();
        (Agent::new(Box::new(provider.clone()), tools), provider)
    }

    #[tokio::test]
    async fn run_stream_executes_a_tool_and_preserves_assistant_text_and_history() {
        let (mut agent, provider) = agent_with_responses(vec![
            vec![
                Ok(ModelEvent::AssistantTextDelta("Checking. ".to_string())),
                Ok(ModelEvent::AssistantMessageDone("Checking. ".to_string())),
                Ok(ModelEvent::ToolCallRequestReady(tool_call())),
                Ok(ModelEvent::ResponseCompleted),
            ],
            vec![
                Ok(ModelEvent::AssistantTextDelta("Done.".to_string())),
                Ok(ModelEvent::AssistantMessageDone("Done.".to_string())),
                Ok(ModelEvent::ResponseCompleted),
            ],
        ]);

        let events = agent
            .run_stream(
                "check",
                AgentRunOptions::from_model_options(ModelOptions::default()),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(events.iter().any(|event| matches!(
            event,
            Ok(AgentEvent::ToolCallStarted { name, .. }) if name == "echo"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            Ok(AgentEvent::ToolCallFinished { result, .. }) if result.success
        )));
        assert!(matches!(events.last(), Some(Ok(AgentEvent::TurnFinished))));

        assert_eq!(agent.history.len(), 5);
        assert!(matches!(
            &agent.history[1],
            Message::AssistantText(content) if content == "Checking. "
        ));
        assert!(matches!(
            &agent.history[2],
            Message::ToolCall(tool_call) if tool_call.id == "call_1"
        ));
        assert!(matches!(agent.history[3], Message::ToolResult { .. }));
        assert!(matches!(
            &agent.history[4],
            Message::AssistantText(content) if content == "Done."
        ));

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].tools[0].name, "echo");
        assert_eq!(requests[1].messages.len(), 4);
        assert!(matches!(
            &requests[1].messages[1],
            Message::AssistantText(content) if content == "Checking. "
        ));
        assert!(matches!(
            &requests[1].messages[2],
            Message::ToolCall(tool_call) if tool_call.id == "call_1"
        ));
    }

    #[tokio::test]
    async fn run_stream_returns_invalid_arguments_to_the_model_as_a_tool_failure() {
        let invalid_call = ToolCall {
            arguments: "not json".to_string(),
            ..tool_call()
        };
        let (mut agent, _) = agent_with_responses(vec![
            vec![
                Ok(ModelEvent::ToolCallRequestReady(invalid_call)),
                Ok(ModelEvent::ResponseCompleted),
            ],
            vec![
                Ok(ModelEvent::AssistantTextDelta("Recovered.".to_string())),
                Ok(ModelEvent::AssistantMessageDone("Recovered.".to_string())),
                Ok(ModelEvent::ResponseCompleted),
            ],
        ]);

        let events = agent
            .run_stream(
                "check",
                AgentRunOptions::from_model_options(ModelOptions::default()),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(events.iter().any(|event| matches!(
            event,
            Ok(AgentEvent::ToolCallFailed { name, .. }) if name == "echo"
        )));
        assert!(matches!(
            &agent.history[2],
            Message::ToolResult { content, .. } if content.contains("invalid tool arguments")
        ));
    }
}
