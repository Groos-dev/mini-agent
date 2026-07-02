use async_stream::stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::{ChatMessage, ChatStream, ProviderError, ReasoningEffort};

use super::OpenAIProvider;

#[derive(Serialize)]
struct CompletionsRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Deserialize)]
struct CompletionsStreamChunk {
    choices: Vec<CompletionsStreamChoice>,
}

#[derive(Deserialize)]
struct CompletionsStreamChoice {
    delta: CompletionsDelta,
}

#[derive(Deserialize)]
struct CompletionsDelta {
    content: Option<String>,
}

impl OpenAIProvider {
    pub(super) async fn chat_stream_completions(
        &self,
        messages: Vec<ChatMessage>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<ChatStream, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = CompletionsRequest {
            model: self.model.clone(),
            messages,
            stream: true,
            reasoning_effort,
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
                    Err(e) => { yield Err(ProviderError::Http(e)); return; }
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
                        if let Ok(parsed) =
                            serde_json::from_str::<CompletionsStreamChunk>(data)
                            && let Some(content) = parsed
                                .choices
                                .into_iter()
                                .next()
                                .and_then(|c| c.delta.content)
                        {
                            yield Ok(content);
                        }
                    }
                }
            }
        };
        Ok(Box::pin(s))
    }
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
            crate::ApiType::Completions,
        )
    }

    fn messages() -> Vec<ChatMessage> {
        vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }]
    }

    async fn collect_chunks(stream: ChatStream) -> Vec<String> {
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
    async fn completions_stream_yields_delta_content() {
        let server = mock_completions(
            r#"data: {"choices":[{"delta":{"content":"Hel"}}]}

data: {"choices":[{"delta":{"content":"lo"}}]}

data: [DONE]
"#,
        )
        .await;

        let stream = provider(server.uri())
            .chat_stream_completions(messages(), None)
            .await
            .unwrap();

        assert_eq!(collect_chunks(stream).await, vec!["Hel", "lo"]);
    }

    #[tokio::test]
    async fn completions_stream_ignores_chunks_without_content() {
        let server = mock_completions(
            r#"data: {"choices":[]}

data: {"choices":[{"delta":{}}]}

data: not-json

event: ignored

data: {"choices":[{"delta":{"content":"done"}}]}

data: [DONE]
"#,
        )
        .await;

        let stream = provider(server.uri())
            .chat_stream_completions(messages(), None)
            .await
            .unwrap();

        assert_eq!(collect_chunks(stream).await, vec!["done"]);
    }

    #[tokio::test]
    async fn completions_stream_returns_api_error_for_non_success_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server failed"))
            .mount(&server)
            .await;

        let result = provider(server.uri())
            .chat_stream_completions(messages(), None)
            .await;
        let err = match result {
            Ok(_) => panic!("non-success status should fail before returning a stream"),
            Err(err) => err,
        };

        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "server failed");
            }
            other => panic!("expected API error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn completions_request_includes_messages_and_reasoning_effort() {
        let server = mock_completions("data: [DONE]\n").await;

        let stream = provider(server.uri())
            .chat_stream_completions(messages(), Some(ReasoningEffort::Medium))
            .await
            .unwrap();
        let _ = collect_chunks(stream).await;

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();

        assert_eq!(body["model"], "test-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(
            body["messages"],
            json!([{ "role": "user", "content": "hello" }])
        );
    }

    #[tokio::test]
    async fn completions_request_omits_reasoning_effort_when_absent() {
        let server = mock_completions("data: [DONE]\n").await;

        let stream = provider(server.uri())
            .chat_stream_completions(messages(), None)
            .await
            .unwrap();
        let _ = collect_chunks(stream).await;

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();

        assert!(body.get("reasoning_effort").is_none());
    }
}
