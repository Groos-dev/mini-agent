use async_stream::stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use tracing::debug;

use agent_protocol::{
    Message, ModelError, ModelEvent, ModelStream, ReasoningEffort, ToolCall, ToolCallDelta,
    ToolSpec,
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
    text: Option<String>,
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
    #[serde(default)]
    content: Vec<ResponsesContentPart>,
}

#[derive(Deserialize)]
struct ResponsesContentPart {
    #[serde(rename = "type")]
    part_type: String,
    #[serde(default)]
    text: Option<String>,
}

impl OpenAIProvider {
    pub(super) async fn chat_stream_response(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<ModelStream, ModelError> {
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
            let mut arguments_by_output_index = HashMap::new();
            let mut text_by_output_index = HashMap::new();
            let mut tool_call_output_indexes = HashMap::new();
            let mut message_completed = false;
            let mut raw_response = Vec::new();

            loop {
                let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, byte_stream.next()).await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => {
                        yield Err(ModelError::StreamIncomplete(
                            "Responses stream idle timeout".to_string(),
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
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        if message_completed {
                            debug!(provider = "openai_responses", response = %raw_response.join("\n"), "provider stream completed");
                            return;
                        }
                        yield Err(ModelError::StreamIncomplete(
                            "Responses ended before response.completed".to_string(),
                        ));
                        return;
                    }

                    raw_response.push(data.to_string());

                    let event = match serde_json::from_str::<ResponsesStreamEvent>(data) {
                        Ok(event) => event,
                        Err(err) => {
                            yield Err(ModelError::Parse(err.to_string()));
                            return;
                        }
                    };

                    match event.event_type.as_str() {
                        "response.output_text.delta" => {
                            let output_index = match required_output_index(&event) {
                                Ok(index) => index,
                                Err(err) => { yield Err(err); return; }
                            };
                            if let Some(delta) = event.delta {
                                text_by_output_index
                                    .entry(output_index)
                                    .or_insert_with(String::new)
                                    .push_str(&delta);
                                yield Ok(ModelEvent::AssistantTextDelta(delta));
                            }
                        }
                        "response.function_call_arguments.delta" => {
                            let index = match required_output_index(&event) {
                                Ok(index) => index,
                                Err(err) => { yield Err(err); return; }
                            };
                            if let Some(delta) = &event.delta {
                                arguments_by_output_index
                                    .entry(index)
                                    .or_insert_with(String::new)
                                    .push_str(delta);
                            }
                            yield Ok(ModelEvent::ToolCallDelta(ToolCallDelta {
                                index,
                                id: event.call_id,
                                name: event.name,
                                arguments_delta: event.delta,
                            }));
                        }
                        "response.function_call_arguments.done" => {
                            let index = match required_output_index(&event) {
                                Ok(index) => index,
                                Err(err) => { yield Err(err); return; }
                            };
                            if let Some(arguments) = event.arguments {
                                arguments_by_output_index.insert(index, arguments);
                            }
                        }
                        "response.output_text.done" => {
                            let index = match required_output_index(&event) {
                                Ok(index) => index,
                                Err(err) => { yield Err(err); return; }
                            };
                            if let Some(text) = event.text {
                                text_by_output_index.insert(index, text);
                            }
                        }
                        "response.output_item.done" => {
                            match complete_output_item(
                                event,
                                &mut arguments_by_output_index,
                                &mut text_by_output_index,
                                &mut tool_call_output_indexes,
                            ) {
                                Ok(Some(event)) => {
                                    yield Ok(event);
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
                            yield Ok(ModelEvent::ResponseCompleted);
                        }
                        "response.failed" | "response.incomplete" => {
                            yield Err(ModelError::StreamIncomplete(format!(
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
                yield Err(ModelError::StreamIncomplete(
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

fn required_output_index(event: &ResponsesStreamEvent) -> Result<usize, ModelError> {
    event.output_index.ok_or_else(|| {
        ModelError::StreamIncomplete(format!("{} is missing output_index", event.event_type))
    })
}

fn complete_output_item(
    event: ResponsesStreamEvent,
    arguments_by_output_index: &mut HashMap<usize, String>,
    text_by_output_index: &mut HashMap<usize, String>,
    tool_call_output_indexes: &mut HashMap<String, usize>,
) -> Result<Option<ModelEvent>, ModelError> {
    let output_index = required_output_index(&event)?;
    let item = event.item.ok_or_else(|| {
        ModelError::StreamIncomplete("response.output_item.done is missing item".to_string())
    })?;

    match item.item_type.as_str() {
        "function_call" => {
            let cached_arguments = arguments_by_output_index.remove(&output_index);
            let arguments = item.arguments.or(cached_arguments);
            let tool_call = required_tool_call(item.call_id, item.name, arguments)?;
            if let Some(existing_index) = tool_call_output_indexes.get(&tool_call.id) {
                if *existing_index != output_index {
                    return Err(ModelError::Parse(format!(
                        "function call {} was emitted for output indexes {existing_index} and {output_index}",
                        tool_call.id
                    )));
                }
                return Ok(None);
            }
            tool_call_output_indexes.insert(tool_call.id.clone(), output_index);
            Ok(Some(ModelEvent::ToolCallRequestReady(tool_call)))
        }
        "message" => {
            let cached_text = text_by_output_index.remove(&output_index);
            let content = item
                .content
                .into_iter()
                .filter(|part| part.part_type == "output_text")
                .filter_map(|part| part.text)
                .collect::<String>();
            let content = (!content.is_empty()).then_some(content).or(cached_text);
            Ok(content.map(ModelEvent::AssistantMessageDone))
        }
        _ => Ok(None),
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

fn required_tool_call(
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
) -> Result<ToolCall, ModelError> {
    let id = id.ok_or_else(|| {
        ModelError::StreamIncomplete("function call is missing call_id".to_string())
    })?;
    let name = name
        .ok_or_else(|| ModelError::StreamIncomplete("function call is missing name".to_string()))?;
    let arguments = arguments.ok_or_else(|| {
        ModelError::StreamIncomplete("function call is missing arguments".to_string())
    })?;

    Ok(ToolCall {
        id,
        name,
        arguments,
    })
}

fn messages_to_input(messages: &[Message]) -> Vec<Value> {
    let mut input = Vec::new();

    for message in messages {
        match message {
            Message::System(content) => {
                input.push(json!({ "role": "system", "content": content }));
            }
            Message::User(content) => {
                input.push(json!({ "role": "user", "content": content }));
            }
            Message::AssistantText(content) => {
                input.push(json!({ "role": "assistant", "content": content }));
            }
            Message::ToolCall(tool_call) => input.push(json!({
                "type": "function_call",
                "call_id": tool_call.id,
                "name": tool_call.name,
                "arguments": tool_call.arguments,
            })),
            Message::ToolResult {
                tool_call_id,
                content,
            } => input.push(json!({
                "type": "function_call_output",
                "call_id": tool_call_id,
                "output": content,
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

    fn messages() -> Vec<Message> {
        vec![Message::system("be concise"), Message::user("hello")]
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
        messages.push(Message::assistant_text("Checking"));
        messages.push(Message::tool_call(ToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: r#"{"command":"pwd"}"#.to_string(),
        }));
        messages.push(Message::tool_result("call_1", "output"));

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
            r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"Checking "}

data: {"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"command\":\"pwd\"}"}

data: {"type":"response.function_call_arguments.done","output_index":1,"call_id":"call_1","name":"shell","arguments":"{\"command\":\"pwd\"}"}

data: {"type":"response.output_item.done","output_index":0,"item":{"type":"message","content":[{"type":"output_text","text":"Checking "}]}}

data: {"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"call_1","name":"shell","arguments":"{\"command\":\"pwd\"}"}}

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
            ModelEvent::AssistantTextDelta(text) if text == "Checking "
        ));
        assert!(matches!(
            &events[1],
            ModelEvent::ToolCallDelta(delta) if delta.arguments_delta.as_deref() == Some(r#"{"command":"pwd"}"#)
        ));
        assert!(matches!(
            &events[2],
            ModelEvent::AssistantMessageDone(text) if text == "Checking "
        ));
        assert!(matches!(
            &events[3],
            ModelEvent::ToolCallRequestReady(call) if call.id == "call_1" && call.name == "shell"
        ));
        assert!(matches!(events[4], ModelEvent::ResponseCompleted));
        assert_eq!(events.len(), 5);
    }

    #[tokio::test]
    async fn responses_stream_rejects_events_missing_output_index() {
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

        assert!(matches!(
            events.last(),
            Some(Err(ModelError::StreamIncomplete(message))) if message.contains("output_index")
        ));
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
                .filter(|event| matches!(event, ModelEvent::ToolCallRequestReady(_)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn responses_stream_rejects_a_call_id_reused_for_another_output_item() {
        let server = mock_responses(
            r#"data: {"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"shell","arguments":"{}"}}

data: {"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"call_1","name":"shell","arguments":"{}"}}

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

        assert!(matches!(
            events.last(),
            Some(Err(ModelError::Parse(message))) if message.contains("call_1")
        ));
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
            ModelError::Api {
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
            Some(Err(ModelError::StreamIncomplete(message))) => {
                message.contains("response.incomplete")
            }
            _ => false,
        });
    }
}
