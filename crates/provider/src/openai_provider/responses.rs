use async_stream::stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::{ChatMessage, ChatStream, ProviderError, ReasoningEffort};

use super::OpenAIProvider;

#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    input: String,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponsesReasoning>,
}

#[derive(Serialize)]
struct ResponsesReasoning {
    effort: ReasoningEffort,
}

#[derive(Deserialize)]
struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<String>,
}

impl OpenAIProvider {
    pub(super) async fn chat_stream_response(
        &self,
        messages: Vec<ChatMessage>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<ChatStream, ProviderError> {
        let url = format!("{}/responses", self.base_url);
        let body = ResponsesRequest {
            model: self.model.clone(),
            input: messages_to_input(&messages),
            stream: true,
            reasoning: reasoning_effort.map(|effort| ResponsesReasoning { effort }),
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
            let mut buf = String::new();

            while let Some(chunk) = byte_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {yield Err(ProviderError::Http(e)); return; }
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return;
                        }
                        if let Ok(event) = serde_json::from_str::<ResponsesStreamEvent>(data)
                            && event.event_type == "response.output_text.delta"
                            && let Some(delta) = event.delta
                        {
                            yield Ok(delta);
                        }
                    }
                }
            }
        };
        Ok(Box::pin(s))
    }
}

fn messages_to_input(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
            ChatMessage {
                role: "system".to_string(),
                content: "be concise".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            },
        ]
    }

    async fn collect_chunks(stream: ChatStream) -> Vec<String> {
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
    fn messages_to_input_formats_messages_in_order() {
        assert_eq!(messages_to_input(&[]), "");
        assert_eq!(
            messages_to_input(&messages()),
            "system: be concise\nuser: hello"
        );
    }

    #[tokio::test]
    async fn responses_stream_yields_output_text_deltas() {
        let server = mock_responses(
            r#"data: {"type":"response.output_text.delta","delta":"Hel"}

data: {"type":"response.output_text.delta","delta":"lo"}

data: [DONE]
"#,
        )
        .await;

        let stream = provider(server.uri())
            .chat_stream_response(messages(), None)
            .await
            .unwrap();

        assert_eq!(collect_chunks(stream).await, vec!["Hel", "lo"]);
    }

    #[tokio::test]
    async fn responses_stream_ignores_non_text_delta_events() {
        let server = mock_responses(
            r#"data: {"type":"response.created"}

data: {"type":"response.output_text.delta"}

data: not-json

event: ignored

data: {"type":"response.output_text.delta","delta":"done"}

data: [DONE]
"#,
        )
        .await;

        let stream = provider(server.uri())
            .chat_stream_response(messages(), None)
            .await
            .unwrap();

        assert_eq!(collect_chunks(stream).await, vec!["done"]);
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
            .chat_stream_response(messages(), None)
            .await;
        let err = match result {
            Ok(_) => panic!("non-success status should fail before returning a stream"),
            Err(err) => err,
        };

        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message, "bad key");
            }
            other => panic!("expected API error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn responses_request_includes_input_and_reasoning_when_present() {
        let server = mock_responses("data: [DONE]\n").await;

        let stream = provider(server.uri())
            .chat_stream_response(messages(), Some(ReasoningEffort::High))
            .await
            .unwrap();
        let _ = collect_chunks(stream).await;

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();

        assert_eq!(body["model"], "test-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["input"], "system: be concise\nuser: hello");
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
    }

    #[tokio::test]
    async fn responses_request_omits_reasoning_when_absent() {
        let server = mock_responses("data: [DONE]\n").await;

        let stream = provider(server.uri())
            .chat_stream_response(messages(), None)
            .await
            .unwrap();
        let _ = collect_chunks(stream).await;

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();

        assert!(body.get("reasoning").is_none());
    }
}
