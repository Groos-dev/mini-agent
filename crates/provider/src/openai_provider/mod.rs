mod completions;
mod responses;

use reqwest::Client;
use std::time::Duration;

use crate::{ChatRequest, ChatStream, Provider, ProviderError, provider::ApiType};

pub struct OpenAIProvider {
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    api_type: ApiType,
    pub(crate) client: Client,
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream, ProviderError> {
        match self.api_type {
            ApiType::Completions => {
                self.chat_stream_completions(
                    request.messages,
                    request.tools,
                    request.options.reasoning_effort,
                )
                .await
            }
            ApiType::Responses => {
                self.chat_stream_response(
                    request.messages,
                    request.tools,
                    request.options.reasoning_effort,
                )
                .await
            }
        }
    }
}

impl OpenAIProvider {
    pub fn new(api_key: String, base_url: String, model: String, api_type: ApiType) -> Self {
        let client = Client::builder()
            .user_agent("mini-agent")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build http client");

        Self {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            api_type,
            client,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> ChatRequest {
        ChatRequest {
            messages: vec![crate::ChatMessage::text(crate::ChatRole::User, "hello")],
            tools: Vec::new(),
            options: crate::ChatOptions::default(),
        }
    }

    fn provider(base_url: String, api_type: ApiType) -> OpenAIProvider {
        OpenAIProvider::new(
            "test-key".to_string(),
            base_url,
            "test-model".to_string(),
            api_type,
        )
    }

    #[test]
    fn provider_normalizes_base_url_trailing_slashes() {
        let provider = provider("https://example.test/v1///".to_string(), ApiType::Responses);
        assert_eq!(provider.base_url, "https://example.test/v1");
    }

    #[tokio::test]
    async fn chat_stream_dispatches_to_completions_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"data: {"choices":[{"delta":{"content":"ok"}}]}

data: {"choices":[{"delta":{},"finish_reason":"stop"}]}

data: [DONE]
"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let stream = provider(server.uri(), ApiType::Completions)
            .chat_stream(request())
            .await
            .unwrap();
        let chunks: Vec<_> = stream.collect::<Vec<_>>().await;

        assert!(matches!(
            chunks.first(),
            Some(Ok(crate::ChatEvent::TextChunk(text))) if text == "ok"
        ));
    }

    #[tokio::test]
    async fn chat_stream_dispatches_to_responses_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"data: {"type":"response.output_text.delta","delta":"ok"}

data: {"type":"response.completed"}

data: [DONE]
"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let stream = provider(server.uri(), ApiType::Responses)
            .chat_stream(request())
            .await
            .unwrap();
        let chunks: Vec<_> = stream.collect::<Vec<_>>().await;

        assert!(matches!(
            chunks.first(),
            Some(Ok(crate::ChatEvent::TextChunk(text))) if text == "ok"
        ));
    }
}
