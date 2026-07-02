mod completions;
mod responses;

use reqwest::Client;

use crate::{ChatRequest, ChatStream, Provider, ProviderError, provider::ApiType};

pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
    model: String,
    api_type: ApiType,
    client: Client,
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream, ProviderError> {
        match self.api_type {
            ApiType::Completions => {
                self.chat_stream_completions(request.messages, request.options.reasoning_effort)
                    .await
            }
            ApiType::Responses => {
                self.chat_stream_response(request.messages, request.options.reasoning_effort)
                    .await
            }
        }
    }
}

impl OpenAIProvider {
    pub fn new(api_key: String, base_url: String, model: String, api_type: ApiType) -> Self {
        let client = Client::builder()
            .user_agent("mini-agent")
            .build()
            .expect("failed to build http client");

        Self {
            api_key,
            base_url,
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
            messages: vec![crate::ChatMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
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

    #[tokio::test]
    async fn chat_stream_dispatches_to_completions_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"data: {"choices":[{"delta":{"content":"ok"}}]}

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

        assert_eq!(
            chunks.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            vec!["ok"]
        );
    }

    #[tokio::test]
    async fn chat_stream_dispatches_to_responses_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"data: {"type":"response.output_text.delta","delta":"ok"}

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

        assert_eq!(
            chunks.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            vec!["ok"]
        );
    }
}
