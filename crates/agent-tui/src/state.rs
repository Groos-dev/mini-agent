use agent_core::agent::AgentEvent;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
    Error,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UiMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RunState {
    Idle,
    Streaming,
}

pub struct AppState {
    model: String,
    api_type: String,
    messages: Vec<UiMessage>,
    composer: String,
    composer_cursor: usize,
    run_state: RunState,
    scroll_offset: usize,
    follow_mode: bool,
    unread_count: usize,
    viewport_height: usize,
    total_message_lines: usize,
}

impl AppState {
    pub fn new(model: impl Into<String>, api_type: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_type: api_type.into(),
            messages: Vec::new(),
            composer: String::new(),
            composer_cursor: 0,
            run_state: RunState::Idle,
            scroll_offset: 0,
            follow_mode: true,
            unread_count: 0,
            viewport_height: 1,
            total_message_lines: 0,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn api_type(&self) -> &str {
        &self.api_type
    }

    pub fn messages(&self) -> &[UiMessage] {
        &self.messages
    }

    pub fn composer(&self) -> &str {
        &self.composer
    }

    pub fn composer_cursor(&self) -> usize {
        self.composer_cursor
    }

    pub fn composer_line_count(&self) -> usize {
        self.composer.lines().count().max(1)
    }

    pub fn run_state(&self) -> RunState {
        self.run_state
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn follow_mode(&self) -> bool {
        self.follow_mode
    }

    pub fn unread_count(&self) -> usize {
        self.unread_count
    }

    pub fn message_scroll_top(&self) -> usize {
        self.total_message_lines
            .saturating_sub(self.viewport_height)
            .saturating_sub(self.scroll_offset)
    }

    pub fn insert_text(&mut self, text: &str) {
        self.composer.insert_str(self.composer_cursor, text);
        self.composer_cursor += text.len();
    }

    pub fn insert_newline(&mut self) {
        self.insert_text("\n");
    }

    pub fn backspace(&mut self) {
        if self.composer_cursor == 0 {
            return;
        }

        let previous = previous_char_boundary(&self.composer, self.composer_cursor);
        self.composer.drain(previous..self.composer_cursor);
        self.composer_cursor = previous;
    }

    pub fn delete(&mut self) {
        if self.composer_cursor == self.composer.len() {
            return;
        }

        let next = next_char_boundary(&self.composer, self.composer_cursor);
        self.composer.drain(self.composer_cursor..next);
    }

    pub fn move_cursor_left(&mut self) {
        self.composer_cursor = previous_char_boundary(&self.composer, self.composer_cursor);
    }

    pub fn move_cursor_right(&mut self) {
        self.composer_cursor = next_char_boundary(&self.composer, self.composer_cursor);
    }

    pub fn move_cursor_home(&mut self) {
        self.composer_cursor = self
            .composer
            .get(..self.composer_cursor)
            .and_then(|prefix| prefix.rfind('\n'))
            .map_or(0, |index| index + 1);
    }

    pub fn move_cursor_end(&mut self) {
        self.composer_cursor = self
            .composer
            .get(self.composer_cursor..)
            .and_then(|suffix| suffix.find('\n'))
            .map_or(self.composer.len(), |index| self.composer_cursor + index);
    }

    pub fn submit_composer(&mut self) -> Option<String> {
        let prompt = self.composer.trim().to_string();
        if prompt.is_empty() {
            return None;
        }

        self.messages.push(UiMessage {
            role: MessageRole::User,
            content: prompt.clone(),
        });
        self.clear_composer();
        self.on_new_message();
        Some(prompt)
    }

    pub fn clear_composer(&mut self) {
        self.composer.clear();
        self.composer_cursor = 0;
    }

    pub fn begin_streaming(&mut self) {
        self.run_state = RunState::Streaming;
    }

    pub fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextChunk(text) => self.append_assistant_text(&text),
            AgentEvent::ToolCallStarted {
                id,
                name,
                arguments,
            } => self.add_tool_message(id, format!("[tool:{name}] started {arguments}")),
            AgentEvent::ToolCallFinished { id, name, result } => {
                self.update_tool_message(
                    &id,
                    format!("[tool:{name}] finished success={}", result.success),
                );
            }
            AgentEvent::ToolCallFailed { id, name, error } => {
                self.update_tool_message(&id, format!("[tool:{name}] failed {error}"));
            }
            AgentEvent::TurnFinished => self.run_state = RunState::Idle,
        }
    }

    pub fn record_stream_error(&mut self, error: impl Into<String>) {
        self.messages.push(UiMessage {
            role: MessageRole::Error,
            content: error.into(),
        });
        self.run_state = RunState::Idle;
        self.on_new_message();
    }

    pub fn set_message_metrics(&mut self, total_lines: usize, viewport_height: usize) {
        self.total_message_lines = total_lines;
        self.viewport_height = viewport_height.max(1);
        self.clamp_scroll();
    }

    pub fn scroll_up(&mut self, lines: usize) {
        if lines == 0 {
            return;
        }

        self.follow_mode = false;
        self.scroll_offset = (self.scroll_offset + lines).min(self.max_scroll());
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if self.scroll_offset == 0 {
            self.follow_mode = true;
            self.unread_count = 0;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.follow_mode = true;
        self.unread_count = 0;
    }

    fn append_assistant_text(&mut self, text: &str) {
        if let Some(message) = self.messages.last_mut()
            && message.role == MessageRole::Assistant
        {
            message.content.push_str(text);
            if !self.follow_mode {
                self.unread_count = self.unread_count.max(1);
            }
        } else {
            self.messages.push(UiMessage {
                role: MessageRole::Assistant,
                content: text.to_string(),
            });
            self.on_new_message();
        }
    }

    fn add_tool_message(&mut self, id: String, content: String) {
        self.messages.push(UiMessage {
            role: MessageRole::Tool,
            content: format!("{id} {content}"),
        });
        self.on_new_message();
    }

    fn update_tool_message(&mut self, id: &str, content: String) {
        if let Some(message) =
            self.messages.iter_mut().rev().find(|message| {
                message.role == MessageRole::Tool && message.content.starts_with(id)
            })
        {
            message.content = format!("{id} {content}");
        }
    }

    fn on_new_message(&mut self) {
        if self.follow_mode {
            self.scroll_offset = 0;
        } else {
            self.unread_count += 1;
        }
    }

    fn max_scroll(&self) -> usize {
        self.total_message_lines
            .saturating_sub(self.viewport_height)
    }

    fn clamp_scroll(&mut self) {
        if self.follow_mode {
            self.scroll_offset = 0;
        } else {
            self.scroll_offset = self.scroll_offset.min(self.max_scroll());
        }
    }
}

fn previous_char_boundary(text: &str, cursor: usize) -> usize {
    text.get(..cursor)
        .and_then(|prefix| prefix.char_indices().next_back().map(|(index, _)| index))
        .unwrap_or(0)
}

fn next_char_boundary(text: &str, cursor: usize) -> usize {
    text.get(cursor..)
        .and_then(|suffix| {
            suffix
                .chars()
                .next()
                .map(|character| cursor + character.len_utf8())
        })
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use agent_core::{agent::AgentEvent, tool::ToolResult};

    use super::*;

    #[test]
    fn submitting_composer_adds_user_message_and_clears_input() {
        let mut state = AppState::new("gpt-test", "completions");
        state.insert_text("hello agent");

        assert_eq!(state.submit_composer(), Some("hello agent".to_string()));
        assert_eq!(state.composer(), "");
        assert!(matches!(
            state.messages().last(),
            Some(UiMessage {
                role: MessageRole::User,
                content
            }) if content == "hello agent"
        ));
        assert_eq!(state.run_state(), RunState::Idle);
    }

    #[test]
    fn text_chunks_are_accumulated_in_one_assistant_message() {
        let mut state = AppState::new("gpt-test", "completions");
        state.insert_text("hello");
        state.submit_composer();
        state.begin_streaming();

        state.apply_event(AgentEvent::TextChunk("first ".to_string()));
        state.apply_event(AgentEvent::TextChunk("answer".to_string()));
        state.apply_event(AgentEvent::TurnFinished);

        assert_eq!(state.messages().len(), 2);
        assert_eq!(state.messages()[1].content, "first answer");
        assert_eq!(state.run_state(), RunState::Idle);
    }

    #[test]
    fn text_after_tool_call_starts_a_new_assistant_message() {
        let mut state = AppState::new("gpt-test", "completions");
        state.begin_streaming();
        state.apply_event(AgentEvent::TextChunk("before tool".to_string()));
        state.apply_event(AgentEvent::ToolCallStarted {
            id: "call-1".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
        });
        state.apply_event(AgentEvent::TextChunk("after tool".to_string()));

        assert_eq!(state.messages().len(), 3);
        assert_eq!(state.messages()[0].content, "before tool");
        assert_eq!(state.messages()[2].content, "after tool");
    }

    #[test]
    fn tool_events_update_visible_tool_status() {
        let mut state = AppState::new("gpt-test", "responses");
        state.begin_streaming();

        state.apply_event(AgentEvent::ToolCallStarted {
            id: "call-1".to_string(),
            name: "shell".to_string(),
            arguments: "{\"command\":\"pwd\"}".to_string(),
        });
        state.apply_event(AgentEvent::ToolCallFinished {
            id: "call-1".to_string(),
            name: "shell".to_string(),
            result: ToolResult::success("/tmp"),
        });

        assert!(matches!(
            state.messages().last(),
            Some(UiMessage {
                role: MessageRole::Tool,
                content
            }) if content.contains("shell") && content.contains("finished")
        ));
    }

    #[test]
    fn composer_supports_cursor_editing_and_newlines() {
        let mut state = AppState::new("gpt-test", "completions");
        state.insert_text("helo");
        state.move_cursor_left();
        state.insert_text("l");
        state.move_cursor_end();
        state.insert_newline();
        state.insert_text("world");
        state.move_cursor_home();
        state.delete();

        assert_eq!(state.composer(), "hello\norld");
        assert_eq!(state.composer_cursor(), 6);
    }

    #[test]
    fn manual_scroll_stops_following_and_tracks_unread_messages() {
        let mut state = AppState::new("gpt-test", "completions");
        state.insert_text("question");
        state.submit_composer();
        state.begin_streaming();
        state.apply_event(AgentEvent::TextChunk(
            "line one\nline two\nline three".to_string(),
        ));
        state.set_message_metrics(3, 2);
        state.scroll_up(1);

        state.apply_event(AgentEvent::TextChunk("\nline four".to_string()));

        assert!(!state.follow_mode());
        assert_eq!(state.unread_count(), 1);
        state.scroll_to_bottom();
        assert!(state.follow_mode());
        assert_eq!(state.unread_count(), 0);
    }

    #[test]
    fn stream_failure_adds_error_and_returns_to_idle() {
        let mut state = AppState::new("gpt-test", "completions");
        state.begin_streaming();
        state.record_stream_error("network unavailable");

        assert_eq!(state.run_state(), RunState::Idle);
        assert!(matches!(
            state.messages().last(),
            Some(UiMessage {
                role: MessageRole::Error,
                content
            }) if content == "network unavailable"
        ));
    }
}
