//! One model interaction: the request, the streamed events and the final answer.

use harness_core::{Message, TokenUsage, ToolCall, ToolSpec};

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Reasoning effort, already validated against
    /// `harness_core::SUPPORTED_THINKING_EFFORTS`.
    ///
    /// `None` means the field is left off the wire entirely: a model that does
    /// not reason has no use for it, and some endpoints reject an unknown key.
    pub reasoning_effort: Option<String>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
        }
    }
}

/// Incremental output from a streaming call.
///
/// Deltas are advisory: the authoritative answer is the [`ProviderResponse`].
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    ThinkingDelta(String),
    TextDelta(String),
    ToolCall(ToolCall),
    Usage(TokenUsage),
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub message: Message,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub finish_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_request_round_trips_through_json() {
        let mut request = ChatRequest::new(
            "gpt-4o-mini",
            vec![
                Message::system("be terse"),
                Message::user("hello"),
                Message::assistant_tool_calls(vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: json!({ "path": "src/main.rs" }),
                }]),
                Message::tool_result("call_1", "fn main() {}"),
            ],
        );
        request.tools = vec![ToolSpec::new(
            "read_file",
            "reads a file",
            harness_core::object_schema(json!({ "path": { "type": "string" } }), &["path"]),
        )];
        request.temperature = Some(0.2);
        request.max_tokens = Some(512);

        let encoded = serde_json::to_string(&request.messages).unwrap();
        let decoded: Vec<Message> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, request.messages);

        let encoded = serde_json::to_string(&request.tools).unwrap();
        let decoded: Vec<ToolSpec> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, request.tools);
    }
}
