use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_user_sets_role_and_content() {
        let message = Message::user("hello");

        assert!(matches!(message.role, Role::User));
        assert_eq!(message.content, "hello");
    }

    #[test]
    fn message_assistant_sets_role_and_content() {
        let message = Message::assistant("hi");

        assert!(matches!(message.role, Role::Assistant));
        assert_eq!(message.content, "hi");
    }
}
