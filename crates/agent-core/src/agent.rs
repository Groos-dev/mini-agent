use async_stream::stream;
use futures::StreamExt;
use provider::{
    ChatEvent, ChatMessage, ChatOptions, ChatRequest, ChatRole, ChatStream, Provider, ToolCall,
};
use std::pin::Pin;
use tracing::{debug, info, warn};

use crate::{
    error::CoreError,
    message::Message,
    tool::{ToolApproval, ToolError, ToolExecutionContext, ToolRegistry, ToolResult},
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
    pub chat_options: ChatOptions,
    pub tool_context: ToolExecutionContext,
    pub max_tool_rounds: usize,
    pub tool_approval: Option<ToolApproval>,
}

impl AgentRunOptions {
    pub fn from_chat_options(chat_options: ChatOptions) -> Self {
        Self {
            chat_options,
            tool_context: ToolExecutionContext::default(),
            max_tool_rounds: 8,
            tool_approval: None,
        }
    }
}

pub struct Agent {
    provider: Box<dyn Provider>,
    history: Vec<Message>,
    tool_registry: ToolRegistry,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tool_registry: ToolRegistry) -> Self {
        Self {
            provider,
            history: Vec::new(),
            tool_registry,
        }
    }

    fn build_request_from_messages(messages: &[Message]) -> Vec<ChatMessage> {
        messages.iter().map(message_to_chat_message).collect()
    }

    pub async fn run_stream(
        &mut self,
        user_input: impl Into<String>,
        options: AgentRunOptions,
    ) -> Result<AgentEventStream<'_>, CoreError> {
        let user_input = user_input.into();
        let mut working_history = self.history.clone();
        working_history.push(Message::user(user_input));
        let registered_tool_count = self.tool_registry.specs().len();

        let s = stream! {
            let mut completed_tool_rounds = 0;
            loop {
                debug!(
                    completed_tool_rounds,
                    history_messages = working_history.len(),
                    registered_tool_count,
                    "requesting model turn"
                );
                let mut provider_stream = match self.run_turn_once(&working_history, &options).await {
                    Ok(stream) => stream,
                    Err(err) => {
                        warn!(error = %err, "failed to start model stream");
                        yield Err(err);
                        return;
                    }
                };

                let mut assistant_text = String::new();
                let mut tool_calls = Vec::new();
                while let Some(event) = provider_stream.next().await {
                    match event {
                        Ok(ChatEvent::TextChunk(text)) => {
                            assistant_text.push_str(&text);
                            yield Ok(AgentEvent::TextChunk(text));
                        }
                        Ok(ChatEvent::ToolCallDone(tool_call)) => {
                            tool_calls.push(tool_call);
                        }
                        Ok(ChatEvent::ToolCallDelta(_)) => {
                            // TODO: 如果需要实时展示 tool call 参数增量，可在这里转换为 AgentEvent。
                        }
                        Err(err) => {
                            warn!(error = %err, "model stream failed");
                            yield Err(CoreError::Provider(err));
                            return;
                        }
                    }
                }

                self.record_assistant_message(
                    &mut working_history,
                    (!assistant_text.is_empty()).then_some(assistant_text),
                    &tool_calls,
                );

                if tool_calls.is_empty() {
                    self.history = working_history;
                    debug!(history_messages = self.history.len(), "agent turn completed");
                    yield Ok(AgentEvent::TurnFinished);
                    return;
                }

                completed_tool_rounds += 1;
                info!(
                    completed_tool_rounds,
                    tool_call_count = tool_calls.len(),
                    "received tool calls"
                );
                if completed_tool_rounds > options.max_tool_rounds {
                    warn!(
                        completed_tool_rounds,
                        max_tool_rounds = options.max_tool_rounds,
                        "tool-call round limit exceeded"
                    );
                    yield Err(CoreError::ToolCallLimitExceeded {
                        limit: options.max_tool_rounds,
                    });
                    return;
                }

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
                            working_history.push(Message::tool_result(
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
                            working_history.push(Message::tool_result(
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
    ) -> Result<ChatStream, CoreError> {
        let request = ChatRequest {
            messages: Self::build_request_from_messages(messages),
            tools: self.tool_registry.specs(),
            options: options.chat_options.clone(),
        };
        Ok(self.provider.chat_stream(request).await?)
    }

    fn record_assistant_message(
        &self,
        working_history: &mut Vec<Message>,
        assistant_text: Option<String>,
        tool_calls: &[ToolCall],
    ) {
        if !tool_calls.is_empty() {
            working_history.push(Message::assistant_response(
                assistant_text,
                tool_calls.to_vec(),
            ));
        } else if let Some(assistant_text) = assistant_text {
            working_history.push(Message::assistant(assistant_text));
        }
    }

    async fn execute_tool_call(
        &self,
        tool_call: &ToolCall,
        options: &AgentRunOptions,
    ) -> Result<ToolResult, CoreError> {
        let input = self.parse_tool_arguments(tool_call)?;
        if let Some(approval) = &options.tool_approval
            && !approval(&tool_call.name, &input)
        {
            return Err(CoreError::Tool(ToolError::Denied(
                "tool call was not approved by the user".to_string(),
            )));
        }
        Ok(self
            .tool_registry
            .execute(&tool_call.name, input, options.tool_context.clone())
            .await?)
    }

    fn parse_tool_arguments(&self, tool_call: &ToolCall) -> Result<serde_json::Value, CoreError> {
        serde_json::from_str(&tool_call.arguments).map_err(|err| CoreError::InvalidToolArguments {
            tool_name: tool_call.name.clone(),
            message: err.to_string(),
        })
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

fn message_to_chat_message(message: &Message) -> ChatMessage {
    match message {
        Message::System(content) => ChatMessage::text(ChatRole::System, content),
        Message::User(content) => ChatMessage::text(ChatRole::User, content),
        Message::Assistant {
            content,
            tool_calls,
        } => ChatMessage {
            role: ChatRole::Assistant,
            content: content.clone(),
            tool_calls: tool_calls.clone(),
            tool_call_id: None,
        },
        Message::ToolResult {
            tool_call_id,
            content,
        } => ChatMessage::tool_result(tool_call_id, content),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use futures::{StreamExt, stream};
    use provider::{ProviderError, ToolSpec};

    use super::*;
    use crate::tool::{ToolError, ToolExecutor};

    type FakeEvents = Vec<Result<ChatEvent, ProviderError>>;
    type FakeResponseQueue = VecDeque<FakeEvents>;

    #[derive(Clone)]
    struct FakeProvider {
        requests: Arc<Mutex<Vec<ChatRequest>>>,
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
    impl Provider for FakeProvider {
        async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream, ProviderError> {
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
            _ctx: ToolExecutionContext,
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
        tools.register(Arc::new(EchoTool)).unwrap();
        (Agent::new(Box::new(provider.clone()), tools), provider)
    }

    #[tokio::test]
    async fn run_stream_executes_a_tool_and_preserves_assistant_text_and_history() {
        let (mut agent, provider) = agent_with_responses(vec![
            vec![
                Ok(ChatEvent::TextChunk("Checking. ".to_string())),
                Ok(ChatEvent::ToolCallDone(tool_call())),
            ],
            vec![Ok(ChatEvent::TextChunk("Done.".to_string()))],
        ]);

        let events = agent
            .run_stream(
                "check",
                AgentRunOptions::from_chat_options(ChatOptions::default()),
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

        assert_eq!(agent.history.len(), 4);
        assert!(matches!(
            &agent.history[1],
            Message::Assistant { content: Some(content), tool_calls }
                if content == "Checking. " && tool_calls[0].id == "call_1"
        ));
        assert!(matches!(agent.history[2], Message::ToolResult { .. }));
        assert!(matches!(
            &agent.history[3],
            Message::Assistant { content: Some(content), tool_calls }
                if content == "Done." && tool_calls.is_empty()
        ));

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].tools[0].name, "echo");
        assert_eq!(requests[1].messages.len(), 3);
        assert_eq!(
            requests[1].messages[1].content.as_deref(),
            Some("Checking. ")
        );
        assert_eq!(requests[1].messages[1].tool_calls[0].id, "call_1");
    }

    #[tokio::test]
    async fn run_stream_returns_invalid_arguments_to_the_model_as_a_tool_failure() {
        let invalid_call = ToolCall {
            arguments: "not json".to_string(),
            ..tool_call()
        };
        let (mut agent, _) = agent_with_responses(vec![
            vec![Ok(ChatEvent::ToolCallDone(invalid_call))],
            vec![Ok(ChatEvent::TextChunk("Recovered.".to_string()))],
        ]);

        let events = agent
            .run_stream(
                "check",
                AgentRunOptions::from_chat_options(ChatOptions::default()),
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

    #[tokio::test]
    async fn run_stream_does_not_execute_unapproved_tool_calls() {
        let (mut agent, _provider) = agent_with_responses(vec![
            vec![Ok(ChatEvent::ToolCallDone(tool_call()))],
            vec![Ok(ChatEvent::TextChunk("Denied.".to_string()))],
        ]);
        let mut options = AgentRunOptions::from_chat_options(ChatOptions::default());
        options.tool_approval = Some(Arc::new(|_, _| false));

        let events = agent
            .run_stream("check", options)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(events.iter().any(|event| matches!(
            event,
            Ok(AgentEvent::ToolCallFailed { error, .. })
                if error.contains("not approved by the user")
        )));
    }

    #[tokio::test]
    async fn run_stream_enforces_the_tool_round_limit_without_committing_history() {
        let (mut agent, _) = agent_with_responses(vec![
            vec![Ok(ChatEvent::ToolCallDone(tool_call()))],
            vec![Ok(ChatEvent::ToolCallDone(tool_call()))],
        ]);
        let options = AgentRunOptions {
            max_tool_rounds: 1,
            ..AgentRunOptions::from_chat_options(ChatOptions::default())
        };

        let events = agent
            .run_stream("check", options)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            events.last(),
            Some(Err(CoreError::ToolCallLimitExceeded { limit: 1 }))
        ));
        assert!(agent.history.is_empty());
    }
}
