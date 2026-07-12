use async_stream::stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use tracing::debug;

use crate::{
    ChatEvent, ChatMessage, ChatRole, ChatStream, ProviderError, ReasoningEffort, ToolCall,
    ToolCallDelta, ToolSpec,
};

use super::OpenAIProvider;

const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponsesReasoning>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ResponsesFunctionTool>,
}

#[derive(Serialize)]
struct ResponsesReasoning {
    effort: ReasoningEffort,
}

#[derive(Serialize)]
struct ResponsesFunctionTool {
    #[serde(rename = "type")]
    tool_type: &'static str,
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize)]
struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    output_index: Option<usize>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    item: Option<ResponsesOutputItem>,
}

#[derive(Deserialize)]
struct ResponsesOutputItem {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

impl OpenAIProvider {
    pub(super) async fn chat_stream_response(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<ChatStream, ProviderError> {
        let url = format!("{}/responses", self.base_url);
        let body = ResponsesRequest {
            model: self.model.clone(),
            input: messages_to_input(&messages),
            stream: true,
            reasoning: reasoning_effort.map(|effort| ResponsesReasoning { effort }),
            tools: tools.into_iter().map(ResponsesFunctionTool::from).collect(),
        };

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await?;
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        let s = stream! {
            let mut byte_stream = resp.bytes_stream();
            let mut utf8_pending = Vec::new();
            let mut buf = String::new();
            let mut completed_tool_call_ids = HashSet::new();
            let mut completed_arguments_by_index = HashMap::new();
            let mut message_completed = false;
            let mut raw_response = Vec::new();

            loop {
                let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, byte_stream.next()).await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => {
                        yield Err(ProviderError::StreamIncomplete(
                            "Responses stream idle timeout".to_string(),
                        ));
                        return;
                    }
                };
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => { yield Err(ProviderError::Http(e)); return; }
                };
                match decode_utf8_chunk(&mut utf8_pending, &chunk) {
                    Ok(text) => buf.push_str(&text),
                    Err(err) => { yield Err(err); return; }
                }

                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        if message_completed {
                            debug!(provider = "openai_responses", response = %raw_response.join("\n"), "provider stream completed");
                            return;
                        }
                        yield Err(ProviderError::StreamIncomplete(
                            "Responses ended before response.completed".to_string(),
                        ));
                        return;
                    }

                    raw_response.push(data.to_string());

                    let event = match serde_json::from_str::<ResponsesStreamEvent>(data) {
                        Ok(event) => event,
                        Err(err) => {
                            yield Err(ProviderError::Parse(err.to_string()));
                            return;
                        }
                    };

                    match event.event_type.as_str() {
                        "response.output_text.delta" => {
                            if let Some(delta) = event.delta {
                                yield Ok(ChatEvent::TextChunk(delta));
                            }
                        }
                        "response.function_call_arguments.delta" => {
                            yield Ok(ChatEvent::ToolCallDelta(ToolCallDelta {
                                index: event.output_index.unwrap_or_default(),
                                id: event.call_id,
                                name: event.name,
                                arguments_delta: event.delta,
                            }));
                        }
                        "response.function_call_arguments.done" => {
                            if let (Some(index), Some(arguments)) = (event.output_index, event.arguments) {
                                completed_arguments_by_index.insert(index, arguments);
                            }
                        }
                        "response.output_item.done" => {
                            match complete_tool_call(event, &completed_arguments_by_index) {
                                Ok(Some(tool_call)) if completed_tool_call_ids.insert(tool_call.id.clone()) => {
                                    yield Ok(ChatEvent::ToolCallDone(tool_call));
                                }
                                Ok(_) => {}
                                Err(err) => {
                                    yield Err(err);
                                    return;
                                }
                            }
                        }
                        "response.completed" => {
                            message_completed = true;
                        }
                        "response.failed" | "response.incomplete" => {
                            yield Err(ProviderError::StreamIncomplete(format!(
                                "Responses emitted {}",
                                event.event_type
                            )));
                            return;
                        }
                        _ => {}
                    }
                }
            }

            if !message_completed {
                yield Err(ProviderError::StreamIncomplete(
                    "Responses ended before response.completed".to_string(),
                ));
            } else {
                debug!(provider = "openai_responses", response = %raw_response.join("\n"), "provider stream completed");
            }
        };
        Ok(Box::pin(s))
    }
}

impl From<ToolSpec> for ResponsesFunctionTool {
    fn from(spec: ToolSpec) -> Self {
        Self {
            tool_type: "function",
            name: spec.name,
            description: spec.description,
            parameters: spec.input_schema,
        }
    }
}

fn complete_tool_call(
    event: ResponsesStreamEvent,
    cached_arguments: &HashMap<usize, String>,
) -> Result<Option<ToolCall>, ProviderError> {
    let output_index = event.output_index;
    if let Some(item) = event.item {
        if item.item_type != "function_call" {
            return Ok(None);
        }
        let arguments = item
            .arguments
            .or_else(|| output_index.and_then(|index| cached_arguments.get(&index).cloned()));
        return required_tool_call(item.call_id, item.name, arguments).map(Some);
    }

    Ok(None)
}

fn decode_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> Result<String, ProviderError> {
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
                .map_err(|err| ProviderError::Parse(err.to_string()))?;
            pending.drain(..valid_up_to);
            Ok(text)
        }
        Err(err) => Err(ProviderError::Parse(format!(
            "invalid UTF-8 in SSE stream: {err}"
        ))),
    }
}

fn required_tool_call(
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
) -> Result<ToolCall, ProviderError> {
    let id = id.ok_or_else(|| {
        ProviderError::StreamIncomplete("function call is missing call_id".to_string())
    })?;
    let name = name.ok_or_else(|| {
        ProviderError::StreamIncomplete("function call is missing name".to_string())
    })?;
    let arguments = arguments.ok_or_else(|| {
        ProviderError::StreamIncomplete("function call is missing arguments".to_string())
    })?;

    Ok(ToolCall {
        id,
        name,
        arguments,
    })
}

fn messages_to_input(messages: &[ChatMessage]) -> Vec<Value> {
    let mut input = Vec::new();

    for message in messages {
        match message.role {
            ChatRole::System | ChatRole::User | ChatRole::Assistant => {
                if let Some(content) = &message.content {
                    let role = match message.role {
                        ChatRole::System => "system",
                        ChatRole::User => "user",
                        ChatRole::Assistant => "assistant",
                        ChatRole::Tool => unreachable!("tool messages use function_call_output"),
                    };
                    input.push(json!({ "role": role, "content": content }));
                }

                if matches!(message.role, ChatRole::Assistant) {
                    input.extend(message.tool_calls.iter().map(|tool_call| {
                        json!({
                            "type": "function_call",
                            "call_id": tool_call.id,
                            "name": tool_call.name,
                            "arguments": tool_call.arguments,
                        })
                    }));
                }
            }
            ChatRole::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id,
                "output": message.content,
            })),
        }
    }

    input
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn provider(base_url: String) -> OpenAIProvider {
        OpenAIProvider::new(
            "test-key".to_string(),
            base_url,
            "test-model".to_string(),
            crate::ApiType::Responses,
        )
    }

    fn messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage::text(ChatRole::System, "be concise"),
            ChatMessage::text(ChatRole::User, "hello"),
        ]
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

    async fn collect_events(stream: ChatStream) -> Vec<ChatEvent> {
        stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect()
    }

    async fn mock_responses(body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[test]
    fn messages_to_input_preserves_function_calls_and_outputs() {
        let mut messages = messages();
        messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: Some("Checking".to_string()),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "shell".to_string(),
                arguments: r#"{"command":"pwd"}"#.to_string(),
            }],
            tool_call_id: None,
        });
        messages.push(ChatMessage::tool_result("call_1", "output"));

        assert_eq!(
            messages_to_input(&messages),
            vec![
                json!({"role": "system", "content": "be concise"}),
                json!({"role": "user", "content": "hello"}),
                json!({"role": "assistant", "content": "Checking"}),
                json!({
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "shell",
                    "arguments": r#"{"command":"pwd"}"#,
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "output",
                }),
            ]
        );
    }

    #[tokio::test]
    async fn responses_stream_yields_text_and_complete_function_calls() {
        let server = mock_responses(
            r#"data: {"type":"response.output_text.delta","delta":"Checking "}

data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"command\":\"pwd\"}"}

data: {"type":"response.function_call_arguments.done","call_id":"call_1","name":"shell","arguments":"{\"command\":\"pwd\"}"}

data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"shell","arguments":"{\"command\":\"pwd\"}"}}

data: {"type":"response.completed"}

data: [DONE]
"#,
        )
        .await;

        let events = collect_events(
            provider(server.uri())
                .chat_stream_response(messages(), vec![shell_spec()], None)
                .await
                .unwrap(),
        )
        .await;

        assert!(matches!(
            &events[0],
            ChatEvent::TextChunk(text) if text == "Checking "
        ));
        assert!(matches!(
            &events[1],
            ChatEvent::ToolCallDelta(delta) if delta.arguments_delta.as_deref() == Some(r#"{"command":"pwd"}"#)
        ));
        assert!(matches!(
            &events[2],
            ChatEvent::ToolCallDone(call) if call.id == "call_1" && call.name == "shell"
        ));
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn responses_stream_rejects_function_calls_missing_required_fields() {
        let server = mock_responses(
            r#"data: {"type":"response.function_call_arguments.done","name":"shell","arguments":"{}"}

data: {"type":"response.completed"}

data: [DONE]
"#,
        )
        .await;

        let events = provider(server.uri())
            .chat_stream_response(messages(), vec![shell_spec()], None)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(events.iter().all(Result::is_ok));
    }

    #[tokio::test]
    async fn responses_arguments_done_without_call_id_waits_for_output_item() {
        let server = mock_responses(
            r#"data: {"type":"response.function_call_arguments.done","output_index":0,"arguments":"{\"command\":\"pwd\"}"}

data: {"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"shell"}}

data: {"type":"response.completed"}

data: [DONE]
"#,
        )
        .await;

        let events = collect_events(
            provider(server.uri())
                .chat_stream_response(messages(), vec![shell_spec()], None)
                .await
                .unwrap(),
        )
        .await;

        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ChatEvent::ToolCallDone(_)))
                .count(),
            1
        );
    }

    #[test]
    fn incremental_utf8_decoder_preserves_split_code_points() {
        let mut pending = Vec::new();
        assert_eq!(
            decode_utf8_chunk(&mut pending, "你".as_bytes()).unwrap(),
            "你"
        );
        pending.clear();
        let bytes = "你".as_bytes();
        assert_eq!(decode_utf8_chunk(&mut pending, &bytes[..1]).unwrap(), "");
        assert_eq!(decode_utf8_chunk(&mut pending, &bytes[1..]).unwrap(), "你");
    }

    #[tokio::test]
    async fn responses_request_includes_function_tools_and_reasoning() {
        let server =
            mock_responses("data: {\"type\":\"response.completed\"}\n\ndata: [DONE]\n").await;

        let stream = provider(server.uri())
            .chat_stream_response(messages(), vec![shell_spec()], Some(ReasoningEffort::High))
            .await
            .unwrap();
        let _ = collect_events(stream).await;

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();

        assert_eq!(body["model"], "test-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "shell");
        assert_eq!(
            body["input"],
            json!([
                {"role": "system", "content": "be concise"},
                {"role": "user", "content": "hello"},
            ])
        );
    }

    #[tokio::test]
    async fn responses_stream_returns_api_error_for_non_success_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .mount(&server)
            .await;

        let result = provider(server.uri())
            .chat_stream_response(messages(), Vec::new(), None)
            .await;
        let err = match result {
            Ok(_) => panic!("non-success status should fail before returning a stream"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            ProviderError::Api {
                status: 401,
                message
            } if message == "bad key"
        ));
    }

    #[tokio::test]
    async fn responses_stream_rejects_incomplete_responses() {
        let server =
            mock_responses("data: {\"type\":\"response.incomplete\"}\n\ndata: [DONE]\n").await;

        let events = provider(server.uri())
            .chat_stream_response(messages(), Vec::new(), None)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(match events.last() {
            Some(Err(ProviderError::StreamIncomplete(message))) => {
                message.contains("response.incomplete")
            }
            _ => false,
        });
    }
}
