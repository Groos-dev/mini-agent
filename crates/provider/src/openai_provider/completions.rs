use async_stream::stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

use agent_protocol::{
    Message, ModelError, ModelEvent, ModelStream, ReasoningEffort, ToolCall, ToolCallDelta,
    ToolSpec,
};

use super::OpenAIProvider;

const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other,
}

#[derive(Serialize)]
struct CompletionsRequest {
    model: String,
    messages: Vec<OpenAIChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAITool>,
}

#[derive(Serialize)]
struct OpenAIChatMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OpenAIToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: OpenAIToolCallFunction,
}

#[derive(Serialize)]
struct OpenAIToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct OpenAITool {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: OpenAIFunctionTool,
}

#[derive(Serialize)]
struct OpenAIFunctionTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// Partial representation of a Chat Completions SSE payload.
///
/// A streamed chunk also carries request metadata (`id`, `object`, `created`,
/// and `model`) and may contain a final `usage`-only chunk with no choices.
/// The stream adapter only needs `choices`, so Serde intentionally ignores the
/// remaining fields. Within a choice, `index` and `delta.role` are likewise
/// transport metadata: the former is not needed for text, and the latter is
/// commonly emitted in the first empty content chunk.
#[derive(Deserialize)]
struct CompletionsStreamChunk {
    choices: Vec<CompletionsStreamChoice>,
}

#[derive(Deserialize)]
struct CompletionsStreamChoice {
    delta: CompletionsDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct CompletionsDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<CompletionsToolCallDelta>,
}

#[derive(Deserialize)]
struct CompletionsToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<CompletionsFunctionDelta>,
}

#[derive(Deserialize)]
struct CompletionsFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Default)]
struct ToolCallAccumulator {
    by_index: HashMap<usize, PartialToolCall>,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    fn push_delta(&mut self, delta: CompletionsToolCallDelta) -> ToolCallDelta {
        let entry = self.by_index.entry(delta.index).or_default();

        if let Some(id) = delta.id.clone() {
            entry.id = Some(id);
        }

        let mut name = None;
        let mut arguments_delta = None;
        if let Some(function) = delta.function {
            // Providers may send an empty name in later argument chunks. Keep the
            // non-empty name from the first chunk so the completed call remains executable.
            if let Some(function_name) = function.name.filter(|name| !name.is_empty()) {
                entry.name = Some(function_name.clone());
                name = Some(function_name);
            }
            if let Some(arguments) = function.arguments {
                entry.arguments.push_str(&arguments);
                arguments_delta = Some(arguments);
            }
        }

        ToolCallDelta {
            index: delta.index,
            id: delta.id,
            name,
            arguments_delta,
        }
    }

    fn finish(self) -> Result<Vec<ToolCall>, ModelError> {
        let mut indexed = self.by_index.into_iter().collect::<Vec<_>>();
        indexed.sort_by_key(|(index, _)| *index);
        indexed
            .into_iter()
            .map(|(index, partial)| partial.finish(index))
            .collect()
    }
}

impl PartialToolCall {
    fn finish(self, index: usize) -> Result<ToolCall, ModelError> {
        let id = self.id.ok_or_else(|| {
            ModelError::StreamIncomplete(format!("tool call at index {index} is missing an id"))
        })?;
        let name = self.name.ok_or_else(|| {
            ModelError::StreamIncomplete(format!("tool call at index {index} is missing a name"))
        })?;
        Ok(ToolCall {
            id,
            name,
            arguments: self.arguments,
        })
    }
}

impl OpenAIProvider {
    pub(super) async fn chat_stream_completions(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<ModelStream, ModelError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = CompletionsRequest {
            model: self.model.clone(),
            messages: messages.into_iter().map(OpenAIChatMessage::from).collect(),
            stream: true,
            reasoning_effort,
            tools: tools.into_iter().map(OpenAITool::from).collect(),
        };

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|err| ModelError::Transport(err.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp
                .text()
                .await
                .map_err(|err| ModelError::Transport(err.to_string()))?;
            return Err(ModelError::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        let s = stream! {
            let mut byte_stream = resp.bytes_stream();
            let mut utf8_pending = Vec::new();
            let mut buf = String::new();
            let mut assistant_text = String::new();
            let mut tool_calls = ToolCallAccumulator::default();
            let mut message_completed = false;
            let mut raw_response = Vec::new();

            loop {
                let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, byte_stream.next()).await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => {
                        yield Err(ModelError::StreamIncomplete(
                            "Chat Completions stream idle timeout".to_string(),
                        ));
                        return;
                    }
                };
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(err) => { yield Err(ModelError::Transport(err.to_string())); return; }
                };
                match decode_utf8_chunk(&mut utf8_pending, &chunk) {
                    Ok(text) => buf.push_str(&text),
                    Err(err) => { yield Err(err); return; }
                }

                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            if message_completed {
                                debug!(provider = "openai_chat_completions", response = %raw_response.join("\n"), "provider stream completed");
                                return;
                            }
                            yield Err(ModelError::StreamIncomplete(
                                "Chat Completions ended before a finish reason".to_string(),
                            ));
                            return;
                        }
                        raw_response.push(data.to_string());
                        // Chat Completions event lifecycle:
                        //
                        // 1. The first chunk commonly contains only `delta.role = "assistant"`.
                        //    It establishes the provider role and produces no local model event.
                        // 2. Content and tool-call deltas are streamed immediately for live output.
                        // 3. A choice with `finish_reason` finalizes the accumulated assistant
                        //    text and/or tool calls, then emits `ResponseCompleted`.
                        // 4. Some providers send a final usage-only chunk (`choices: []`), which
                        //    is intentionally ignored before the `[DONE]` stream terminator.
                        match serde_json::from_str::<CompletionsStreamChunk>(data) {
                            Ok(parsed) => {
                                for choice in parsed.choices {
                                    if let Some(content) = choice.delta.content.filter(|content| !content.is_empty()) {
                                        assistant_text.push_str(&content);
                                        yield Ok(ModelEvent::AssistantTextDelta(content));
                                    }

                                    for tool_delta in choice.delta.tool_calls {
                                        let event_delta = tool_calls.push_delta(tool_delta);
                                        yield Ok(ModelEvent::ToolCallDelta(event_delta));
                                    }

                                    if let Some(reason) = choice.finish_reason {
                                        let finish_reason = map_finish_reason(&reason);
                                        match finish_reason {
                                            FinishReason::Stop | FinishReason::ToolCalls => {
                                                if !assistant_text.is_empty() {
                                                    yield Ok(ModelEvent::AssistantMessageDone(
                                                        std::mem::take(&mut assistant_text),
                                                    ));
                                                }
                                                if finish_reason == FinishReason::ToolCalls {
                                                    match std::mem::take(&mut tool_calls).finish() {
                                                        Ok(finished_tool_calls) => {
                                                            for tool_call in finished_tool_calls {
                                                                yield Ok(ModelEvent::ToolCallRequestReady(tool_call));
                                                            }
                                                        }
                                                        Err(err) => {
                                                            yield Err(err);
                                                            return;
                                                        }
                                                    }
                                                }
                                                yield Ok(ModelEvent::ResponseCompleted);
                                                message_completed = true;
                                            }
                                            FinishReason::Length | FinishReason::ContentFilter | FinishReason::Other => {
                                                yield Err(ModelError::StreamIncomplete(format!(
                                                    "Chat Completions finished with {reason}"
                                                )));
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                yield Err(ModelError::Parse(err.to_string()));
                                return;
                            }
                        }
                    }
                }
            }

            if !message_completed {
                yield Err(ModelError::StreamIncomplete(
                    "Chat Completions ended before a finish reason".to_string(),
                ));
            } else {
                debug!(provider = "openai_chat_completions", response = %raw_response.join("\n"), "provider stream completed");
            }
        };
        Ok(Box::pin(s))
    }
}

impl From<Message> for OpenAIChatMessage {
    fn from(message: Message) -> Self {
        match message {
            Message::System(content) => Self {
                role: "system",
                content: Some(content),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            Message::User(content) => Self {
                role: "user",
                content: Some(content),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            Message::AssistantText(content) => Self {
                role: "assistant",
                content: Some(content),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            Message::ToolCall(tool_call) => Self {
                role: "assistant",
                content: None,
                tool_calls: vec![tool_call.into()],
                tool_call_id: None,
            },
            Message::ToolResult {
                tool_call_id,
                content,
            } => Self {
                role: "tool",
                content: Some(content),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool_call_id),
            },
        }
    }
}

impl From<ToolCall> for OpenAIToolCall {
    fn from(tool_call: ToolCall) -> Self {
        Self {
            id: tool_call.id,
            tool_type: "function",
            function: OpenAIToolCallFunction {
                name: tool_call.name,
                arguments: tool_call.arguments,
            },
        }
    }
}

impl From<ToolSpec> for OpenAITool {
    fn from(spec: ToolSpec) -> Self {
        Self {
            tool_type: "function",
            function: OpenAIFunctionTool {
                name: spec.name,
                description: spec.description,
                parameters: spec.input_schema,
            },
        }
    }
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    }
}

fn decode_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> Result<String, ModelError> {
    pending.extend_from_slice(chunk);
    match std::str::from_utf8(pending) {
        Ok(text) => {
            let text = text.to_string();
            pending.clear();
            Ok(text)
        }
        Err(err) if err.error_len().is_none() => {
            let valid_up_to = err.valid_up_to();
            let text = String::from_utf8(pending[..valid_up_to].to_vec())
                .map_err(|err| ModelError::Parse(err.to_string()))?;
            pending.drain(..valid_up_to);
            Ok(text)
        }
        Err(err) => Err(ModelError::Parse(format!(
            "invalid UTF-8 in SSE stream: {err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn provider(base_url: String) -> OpenAIProvider {
        OpenAIProvider::new(
            "test-key".to_string(),
            base_url,
            "test-model".to_string(),
            crate::ApiType::Completions,
        )
    }

    fn messages() -> Vec<Message> {
        vec![Message::user("hello")]
    }

    fn shell_spec() -> ToolSpec {
        ToolSpec {
            name: "shell".to_string(),
            description: "Run a command".to_string(),
            input_schema: json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 120000 },
                },
            }),
        }
    }

    async fn collect_events(stream: ModelStream) -> Vec<ModelEvent> {
        stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect()
    }

    async fn mock_completions(body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn completions_stream_assembles_tool_call_deltas_in_index_order() {
        let server = mock_completions(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_2","function":{"name":"shell","arguments":"{\"command\":\"two\"}"}},{"index":0,"id":"call_1","function":{"name":"shell","arguments":"{\"command\":\"one\"}"}}]}}]}

data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
        )
        .await;

        let events = collect_events(
            provider(server.uri())
                .chat_stream_completions(messages(), vec![shell_spec()], None)
                .await
                .unwrap(),
        )
        .await;

        let calls = events
            .into_iter()
            .filter_map(|event| match event {
                ModelEvent::ToolCallRequestReady(call) => Some(call),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[1].id, "call_2");
    }

    #[tokio::test]
    async fn completions_stream_preserves_tool_name_across_empty_name_deltas() {
        let server = mock_completions(
            r#"data: {"id":"chatcmpl-6c3c952d99e37a508a848e8b","object":"chat.completion.chunk","created":1783957026,"model":"gpt-5.5","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-6c3c952d99e37a508a848e8b","object":"chat.completion.chunk","created":1783957026,"model":"gpt-5.5","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_88be7b4de1614c76d3d2b1b8","type":"function","function":{"name":"shell","arguments":""}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-6c3c952d99e37a508a848e8b","object":"chat.completion.chunk","created":1783957026,"model":"gpt-5.5","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":"{\"command\":\"date"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-6c3c952d99e37a508a848e8b","object":"chat.completion.chunk","created":1783957026,"model":"gpt-5.5","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":"\",\"cwd\":\".\",\"timeout_ms\":1000}"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-6c3c952d99e37a508a848e8b","object":"chat.completion.chunk","created":1783957026,"model":"gpt-5.5","choices":[{"index":0,"delta":{"content":""},"finish_reason":"tool_calls"}]}

data: {"id":"chatcmpl-6c3c952d99e37a508a848e8b","object":"chat.completion.chunk","created":1783957026,"model":"gpt-5.5","choices":[],"usage":{"prompt_tokens":108,"completion_tokens":27,"total_tokens":135}}

data: [DONE]
"#,
        )
        .await;

        let events = collect_events(
            provider(server.uri())
                .chat_stream_completions(messages(), vec![shell_spec()], None)
                .await
                .unwrap(),
        )
        .await;

        assert!(matches!(
            events.as_slice(),
            [
                ModelEvent::ToolCallDelta(_),
                ModelEvent::ToolCallDelta(_),
                ModelEvent::ToolCallDelta(_),
                ModelEvent::ToolCallRequestReady(call),
                ModelEvent::ResponseCompleted,
            ] if call.id == "call_88be7b4de1614c76d3d2b1b8"
                && call.name == "shell"
                && call.arguments == r#"{"command":"date","cwd":".","timeout_ms":1000}"#
        ));
    }

    #[tokio::test]
    async fn completions_stream_emits_common_message_and_completion_events() {
        let server = mock_completions(
            r#"data: {"choices":[{"delta":{"content":"Done."}}]}

data: {"choices":[{"delta":{},"finish_reason":"stop"}]}

data: [DONE]
"#,
        )
        .await;

        let events = collect_events(
            provider(server.uri())
                .chat_stream_completions(messages(), Vec::new(), None)
                .await
                .unwrap(),
        )
        .await;

        assert!(matches!(
            events.as_slice(),
            [
                ModelEvent::AssistantTextDelta(text),
                ModelEvent::AssistantMessageDone(message),
                ModelEvent::ResponseCompleted,
            ] if text == "Done." && message == "Done."
        ));
    }

    #[tokio::test]
    async fn completions_stream_handles_observed_provider_event_sequence() {
        let server = mock_completions(
            r#"data: {"id":"chatcmpl-8612c59bbff965d73bddc492","object":"chat.completion.chunk","created":1783956468,"model":"gpt-5.5","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-8612c59bbff965d73bddc492","object":"chat.completion.chunk","created":1783956468,"model":"gpt-5.5","choices":[{"index":0,"delta":{"content":"com"},"finish_reason":null}]}

data: {"id":"chatcmpl-8612c59bbff965d73bddc492","object":"chat.completion.chunk","created":1783956468,"model":"gpt-5.5","choices":[{"index":0,"delta":{"content":"plet"},"finish_reason":null}]}

data: {"id":"chatcmpl-8612c59bbff965d73bddc492","object":"chat.completion.chunk","created":1783956468,"model":"gpt-5.5","choices":[{"index":0,"delta":{"content":"ions"},"finish_reason":null}]}

data: {"id":"chatcmpl-8612c59bbff965d73bddc492","object":"chat.completion.chunk","created":1783956468,"model":"gpt-5.5","choices":[{"index":0,"delta":{"content":" probe"},"finish_reason":null}]}

data: {"id":"chatcmpl-8612c59bbff965d73bddc492","object":"chat.completion.chunk","created":1783956468,"model":"gpt-5.5","choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-8612c59bbff965d73bddc492","object":"chat.completion.chunk","created":1783956468,"model":"gpt-5.5","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":8,"total_tokens":108}}

data: [DONE]
"#,
        )
        .await;

        let events = collect_events(
            provider(server.uri())
                .chat_stream_completions(messages(), Vec::new(), None)
                .await
                .unwrap(),
        )
        .await;

        assert!(matches!(
            events.as_slice(),
            [
                ModelEvent::AssistantTextDelta(first),
                ModelEvent::AssistantTextDelta(second),
                ModelEvent::AssistantTextDelta(third),
                ModelEvent::AssistantTextDelta(fourth),
                ModelEvent::AssistantMessageDone(message),
                ModelEvent::ResponseCompleted,
            ] if first == "com"
                && second == "plet"
                && third == "ions"
                && fourth == " probe"
                && message == "completions probe"
        ));
    }

    #[tokio::test]
    async fn completions_stream_rejects_tool_calls_missing_required_fields() {
        let server = mock_completions(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"shell","arguments":"{}"}}]}}]}

data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]
"#,
        )
        .await;

        let events = provider(server.uri())
            .chat_stream_completions(messages(), vec![shell_spec()], None)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(match events.last() {
            Some(Err(ModelError::StreamIncomplete(message))) => message.contains("missing an id"),
            _ => false,
        });
    }

    #[test]
    fn incremental_utf8_decoder_preserves_split_code_points() {
        let mut pending = Vec::new();
        let bytes = "你".as_bytes();
        assert_eq!(decode_utf8_chunk(&mut pending, &bytes[..1]).unwrap(), "");
        assert_eq!(decode_utf8_chunk(&mut pending, &bytes[1..]).unwrap(), "你");
    }

    #[tokio::test]
    async fn completions_request_serializes_tool_history_and_schema() {
        let server = mock_completions(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n",
        )
        .await;
        let messages = vec![
            Message::assistant_text("Checking"),
            Message::tool_call(ToolCall {
                id: "call_1".to_string(),
                name: "shell".to_string(),
                arguments: r#"{"command":"pwd"}"#.to_string(),
            }),
            Message::tool_result("call_1", "output"),
        ];

        let stream = provider(server.uri())
            .chat_stream_completions(messages, vec![shell_spec()], Some(ReasoningEffort::Medium))
            .await
            .unwrap();
        let _ = collect_events(stream).await;

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "shell");
        assert_eq!(body["messages"][0]["content"], "Checking");
        assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
    }

    #[tokio::test]
    async fn completions_stream_rejects_missing_finish_reason() {
        let server = mock_completions(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: [DONE]\n",
        )
        .await;

        let events = provider(server.uri())
            .chat_stream_completions(messages(), Vec::new(), None)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(match events.last() {
            Some(Err(ModelError::StreamIncomplete(message))) => {
                message.contains("finish reason")
            }
            _ => false,
        });
    }
}
