use provider::{ChatMessage, ChatOptions, ChatRequest, ChatStream, Provider};

use crate::{
    error::CoreError,
    message::{Message, Role},
};

pub struct Agent {
    provider: Box<dyn Provider>,
    history: Vec<Message>,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>) -> Self {
        Self {
            provider,
            history: Vec::new(),
        }
    }

    fn build_request(&self) -> Vec<ChatMessage> {
        self.history
            .iter()
            .map(|m| ChatMessage {
                role: match &m.role {
                    Role::System => "system".to_string(),
                    Role::User => "user".to_string(),
                    Role::Assistant => "assistant".to_string(),
                },
                content: m.content.clone(),
            })
            .collect()
    }

    pub async fn chat_stream(
        &mut self,
        user_input: impl Into<String>,
        options: ChatOptions,
    ) -> Result<ChatStream, CoreError> {
        let user_input = user_input.into();
        let mut messages = self.build_request();
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: user_input.clone(),
        });

        let request = ChatRequest { messages, options };
        let stream = self.provider.chat_stream(request).await?;
        self.history.push(Message::user(user_input));
        Ok(stream)
    }

    pub fn push_assistant_message(&mut self, content: impl Into<String>) {
        self.history.push(Message::assistant(content));
    }

    pub fn discard_pending_user_message(&mut self) {
        if matches!(self.history.last().map(|m| &m.role), Some(Role::User)) {
            self.history.pop();
        }
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use provider::{ProviderError, ReasoningEffort};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeProvider {
        requests: Arc<Mutex<Vec<ChatRequest>>>,
        result: FakeResult,
    }

    #[derive(Clone)]
    enum FakeResult {
        Stream(Vec<String>),
        Error(String),
    }

    impl FakeProvider {
        fn success(chunks: Vec<&str>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                result: FakeResult::Stream(chunks.into_iter().map(String::from).collect()),
            }
        }

        fn setup_error(message: &str) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                result: FakeResult::Error(message.to_string()),
            }
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Provider for FakeProvider {
        async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream, ProviderError> {
            self.requests.lock().unwrap().push(request);

            match &self.result {
                FakeResult::Stream(chunks) => {
                    let chunks = chunks.clone().into_iter().map(Ok);
                    Ok(Box::pin(stream::iter(chunks)))
                }
                FakeResult::Error(message) => Err(ProviderError::Parse(message.clone())),
            }
        }
    }

    fn assert_message(message: &Message, role: Role, content: &str) {
        assert_eq!(
            std::mem::discriminant(&message.role),
            std::mem::discriminant(&role)
        );
        assert_eq!(message.content, content);
    }

    fn assert_chat_message(message: &ChatMessage, role: &str, content: &str) {
        assert_eq!(message.role, role);
        assert_eq!(message.content, content);
    }

    #[test]
    fn new_agent_starts_with_empty_history() {
        let provider = FakeProvider::success(vec![]);
        let agent = Agent::new(Box::new(provider));

        assert!(agent.history.is_empty());
    }

    #[tokio::test]
    async fn chat_stream_sends_current_history_plus_new_user_message() {
        let provider = FakeProvider::success(vec!["ok"]);
        let requests = provider.clone();
        let mut agent = Agent::new(Box::new(provider));

        let _stream = agent
            .chat_stream("hello", ChatOptions::default())
            .await
            .unwrap();

        let requests = requests.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages.len(), 1);
        assert_chat_message(&requests[0].messages[0], "user", "hello");
    }

    #[tokio::test]
    async fn chat_stream_adds_user_message_to_history_after_provider_accepts_request() {
        let provider = FakeProvider::success(vec!["ok"]);
        let mut agent = Agent::new(Box::new(provider));

        let _stream = agent
            .chat_stream("hello", ChatOptions::default())
            .await
            .unwrap();

        assert_eq!(agent.history.len(), 1);
        assert_message(&agent.history[0], Role::User, "hello");
    }

    #[tokio::test]
    async fn chat_stream_does_not_add_user_message_when_provider_returns_setup_error() {
        let provider = FakeProvider::setup_error("boom");
        let mut agent = Agent::new(Box::new(provider));

        let result = agent.chat_stream("hello", ChatOptions::default()).await;
        let err = match result {
            Ok(_) => panic!("provider setup error should be returned"),
            Err(err) => err,
        };

        assert!(matches!(err, CoreError::Provider(ProviderError::Parse(_))));
        assert!(agent.history.is_empty());
    }

    #[test]
    fn push_assistant_message_appends_assistant_turn() {
        let provider = FakeProvider::success(vec![]);
        let mut agent = Agent::new(Box::new(provider));

        agent.push_assistant_message("answer");

        assert_eq!(agent.history.len(), 1);
        assert_message(&agent.history[0], Role::Assistant, "answer");
    }

    #[tokio::test]
    async fn subsequent_chat_stream_includes_prior_user_and_assistant_history() {
        let provider = FakeProvider::success(vec!["ok"]);
        let requests = provider.clone();
        let mut agent = Agent::new(Box::new(provider));

        let _first_stream = agent
            .chat_stream("first", ChatOptions::default())
            .await
            .unwrap();
        agent.push_assistant_message("reply");
        let _second_stream = agent
            .chat_stream("second", ChatOptions::default())
            .await
            .unwrap();

        let requests = requests.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.len(), 3);
        assert_chat_message(&requests[1].messages[0], "user", "first");
        assert_chat_message(&requests[1].messages[1], "assistant", "reply");
        assert_chat_message(&requests[1].messages[2], "user", "second");
    }

    #[tokio::test]
    async fn discard_pending_user_message_removes_last_user_message() {
        let provider = FakeProvider::success(vec!["ok"]);
        let mut agent = Agent::new(Box::new(provider));

        let _stream = agent
            .chat_stream("hello", ChatOptions::default())
            .await
            .unwrap();
        agent.discard_pending_user_message();

        assert!(agent.history.is_empty());
    }

    #[test]
    fn discard_pending_user_message_does_not_remove_assistant_message() {
        let provider = FakeProvider::success(vec![]);
        let mut agent = Agent::new(Box::new(provider));

        agent.push_assistant_message("answer");
        agent.discard_pending_user_message();

        assert_eq!(agent.history.len(), 1);
        assert_message(&agent.history[0], Role::Assistant, "answer");
    }

    #[tokio::test]
    async fn clear_history_removes_all_messages() {
        let provider = FakeProvider::success(vec!["ok"]);
        let mut agent = Agent::new(Box::new(provider));

        let _stream = agent
            .chat_stream("hello", ChatOptions::default())
            .await
            .unwrap();
        agent.push_assistant_message("answer");
        agent.clear_history();

        assert!(agent.history.is_empty());
    }

    #[tokio::test]
    async fn chat_stream_passes_chat_options_to_provider() {
        let provider = FakeProvider::success(vec!["ok"]);
        let requests = provider.clone();
        let mut agent = Agent::new(Box::new(provider));

        let _stream = agent
            .chat_stream(
                "hello",
                ChatOptions {
                    reasoning_effort: Some(ReasoningEffort::High),
                },
            )
            .await
            .unwrap();

        let requests = requests.requests();
        assert_eq!(
            requests[0].options.reasoning_effort,
            Some(ReasoningEffort::High)
        );
    }
}
