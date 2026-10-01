//! Conversation messages as the harness models them.
//!
//! These types are the harness's own shape, not the wire shape: each provider
//! maps them onto its own protocol. Tool-call arguments are kept as parsed JSON
//! rather than the JSON *string* that OpenAI's wire format uses, because every
//! consumer of a `ToolCall` (validation, execution, logging) wants the object.
//!
//! They live in `harness-core` rather than in the provider crate because memory,
//! the agent loop and the WebSocket protocol all speak in terms of them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }

    /// True for turns that carry conversation rather than mechanical bulk.
    ///
    /// Lossless trimming only ever rewrites tool turns, which is what makes the
    /// trim "lossless" with respect to what the user and the model actually said.
    pub fn is_conversational(&self) -> bool {
        matches!(self, Role::System | Role::User | Role::Assistant)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::with_text(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::with_text(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::with_text(Role::Assistant, content)
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: None,
            tool_calls,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            name: None,
        }
    }

    /// `""` for messages that carry no text, such as a pure tool-call turn.
    pub fn text(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }

    fn with_text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roles_serialise_lowercase() {
        let encoded =
            serde_json::to_string(&[Role::System, Role::User, Role::Assistant, Role::Tool])
                .unwrap();
        assert_eq!(encoded, r#"["system","user","assistant","tool"]"#);
    }

    #[test]
    fn only_conversational_roles_survive_trimming_verbatim() {
        assert!(Role::User.is_conversational());
        assert!(Role::Assistant.is_conversational());
        assert!(Role::System.is_conversational());
        assert!(!Role::Tool.is_conversational());
    }

    #[test]
    fn constructors_set_the_role_and_content() {
        assert_eq!(Message::system("s").role, Role::System);
        assert_eq!(Message::user("u").role, Role::User);
        assert_eq!(Message::assistant("a").text(), "a");

        let call = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: json!({ "path": "a.rs" }),
        };
        let assistant = Message::assistant_tool_calls(vec![call.clone()]);
        assert_eq!(assistant.tool_calls, vec![call]);
        assert_eq!(assistant.text(), "");

        let result = Message::tool_result("call_1", "contents");
        assert_eq!(result.role, Role::Tool);
        assert_eq!(result.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(result.text(), "contents");
    }

    #[test]
    fn message_round_trips_through_json() {
        let original = Message {
            role: Role::Assistant,
            content: Some("calling a tool".into()),
            tool_calls: vec![ToolCall {
                id: "call_9".into(),
                name: "shell".into(),
                arguments: json!({ "cmd": "ls", "timeout": 5 }),
            }],
            tool_call_id: None,
            name: Some("harness".into()),
        };

        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Message = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }
}
