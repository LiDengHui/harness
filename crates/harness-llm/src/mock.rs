//! The offline provider.
//!
//! Every automated test and the no-key demo path runs against this provider, so
//! its behaviour must be deterministic: a script always yields the same deltas
//! and the same token estimate. Text is deliberately split across several
//! deltas so that consumers cannot develop a one-delta-per-turn assumption.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use harness_core::{HarnessError, HeuristicEstimator, Result, TokenEstimator, TokenUsage};

use crate::provider::Provider;
use crate::request::{ChatRequest, ProviderEvent, ProviderResponse};
use harness_core::{Message, Role, ToolCall};

/// Longest text piece the mock ever emits in one delta.
const MAX_DELTA_CHARS: usize = 12;

#[derive(Debug, Clone)]
pub enum ScriptedTurn {
    Text(String),
    Thinking(String),
    ToolCall { name: String, arguments: Value },
    Fail(String),
}

pub struct MockProvider {
    id: String,
    model: String,
    script: Vec<ScriptedTurn>,
    calls: AtomicUsize,
    /// Reasoning effort of each request, in call order. The mock has no
    /// reasoning to spend, so a caller that cares about the level asserts on
    /// what was asked for rather than on what came back.
    efforts: Mutex<Vec<Option<String>>>,
}

impl MockProvider {
    pub fn new(id: impl Into<String>, model: impl Into<String>, script: Vec<ScriptedTurn>) -> Self {
        Self {
            id: id.into(),
            model: model.into(),
            script,
            calls: AtomicUsize::new(0),
            efforts: Mutex::new(Vec::new()),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The reasoning effort of every request this instance has served.
    pub fn recorded_efforts(&self) -> Vec<Option<String>> {
        match self.efforts.lock() {
            Ok(efforts) => efforts.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn record_effort(&self, effort: Option<String>) {
        match self.efforts.lock() {
            Ok(mut efforts) => efforts.push(effort),
            Err(poisoned) => poisoned.into_inner().push(effort),
        }
    }

    fn failure(&self, message: impl Into<String>) -> HarnessError {
        HarnessError::Provider {
            provider: self.id.clone(),
            message: message.into(),
        }
    }

    fn emit(&self, events: &UnboundedSender<ProviderEvent>, event: ProviderEvent) -> Result<()> {
        events
            .send(event)
            .map_err(|_| self.failure("the event sink was closed before the turn finished"))
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn stream(
        &self,
        request: ChatRequest,
        events: UnboundedSender<ProviderEvent>,
    ) -> Result<ProviderResponse> {
        let turn_index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.record_effort(request.reasoning_effort.clone());
        let estimator = HeuristicEstimator::default();
        let input_tokens = estimate_input(&estimator, &request) as u64;

        // Running out of script is a normal end of conversation, not a failure.
        let turn = self
            .script
            .get(turn_index)
            .cloned()
            .unwrap_or(ScriptedTurn::Text(String::new()));

        match turn {
            ScriptedTurn::Fail(message) => Err(self.failure(message)),

            ScriptedTurn::Text(text) => {
                for piece in split_deltas(&text) {
                    self.emit(&events, ProviderEvent::TextDelta(piece))?;
                }
                let usage = TokenUsage::new(input_tokens, estimator.estimate(&text) as u64);
                self.emit(&events, ProviderEvent::Usage(usage))?;

                Ok(ProviderResponse {
                    message: Message::assistant(text),
                    tool_calls: Vec::new(),
                    usage,
                    finish_reason: Some("stop".to_string()),
                })
            }

            ScriptedTurn::Thinking(text) => {
                let usage = TokenUsage::new(input_tokens, estimator.estimate(&text) as u64);
                self.emit(&events, ProviderEvent::ThinkingDelta(text))?;
                self.emit(&events, ProviderEvent::Usage(usage))?;

                Ok(ProviderResponse {
                    // Reasoning is not visible content, so the turn has none.
                    message: Message {
                        role: Role::Assistant,
                        content: None,
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        name: None,
                    },
                    tool_calls: Vec::new(),
                    usage,
                    finish_reason: Some("stop".to_string()),
                })
            }

            ScriptedTurn::ToolCall { name, arguments } => {
                let call = ToolCall {
                    id: format!("call_{turn_index}"),
                    name,
                    arguments,
                };
                let usage = TokenUsage::new(
                    input_tokens,
                    estimator.estimate(&call.arguments.to_string()) as u64,
                );

                self.emit(&events, ProviderEvent::ToolCall(call.clone()))?;
                self.emit(&events, ProviderEvent::Usage(usage))?;

                Ok(ProviderResponse {
                    message: Message::assistant_tool_calls(vec![call.clone()]),
                    tool_calls: vec![call],
                    usage,
                    finish_reason: Some("tool_calls".to_string()),
                })
            }
        }
    }
}

fn estimate_input(estimator: &HeuristicEstimator, request: &ChatRequest) -> usize {
    let rendered: Vec<String> = request
        .messages
        .iter()
        .map(|message| {
            let mut text = message.text().to_string();
            if let Some(name) = &message.name {
                text.push_str(name);
            }
            for call in &message.tool_calls {
                text.push_str(&call.name);
                text.push_str(&call.arguments.to_string());
            }
            text
        })
        .collect();
    let borrows: Vec<&str> = rendered.iter().map(String::as_str).collect();
    estimator.estimate_messages(&borrows)
}

fn split_deltas(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut chars = 0usize;

    for ch in text.chars() {
        current.push(ch);
        chars += 1;
        if chars >= MAX_DELTA_CHARS {
            pieces.push(std::mem::take(&mut current));
            chars = 0;
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn provider(script: Vec<ScriptedTurn>) -> MockProvider {
        MockProvider::new("mock", "mock-model", script)
    }

    fn request() -> ChatRequest {
        ChatRequest::new("mock-model", vec![Message::user("hello there")])
    }

    async fn drain(mut rx: UnboundedReceiver<ProviderEvent>) -> Vec<ProviderEvent> {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn a_text_turn_streams_several_deltas() {
        let text = "streaming is genuinely exercised";
        let provider = provider(vec![ScriptedTurn::Text(text.to_string())]);
        let (tx, rx) = unbounded_channel();

        let response = provider.stream(request(), tx).await.unwrap();
        let events = drain(rx).await;

        let deltas: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::TextDelta(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(deltas.len() > 1, "expected several deltas, got {deltas:?}");
        assert!(deltas
            .iter()
            .all(|delta| delta.chars().count() <= MAX_DELTA_CHARS));
        assert_eq!(deltas.concat(), text);

        assert_eq!(response.message.text(), text);
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
        assert!(response.tool_calls.is_empty());
        assert!(response.usage.input_tokens > 0);
        assert!(response.usage.output_tokens > 0);
        assert!(matches!(
            events.last(),
            Some(ProviderEvent::Usage(usage)) if *usage == response.usage
        ));
    }

    #[tokio::test]
    async fn thinking_is_a_single_delta() {
        let provider = provider(vec![ScriptedTurn::Thinking("considering".into())]);
        let (tx, rx) = unbounded_channel();

        let response = provider.stream(request(), tx).await.unwrap();
        let events = drain(rx).await;

        assert_eq!(
            events[0],
            ProviderEvent::ThinkingDelta("considering".to_string())
        );
        assert!(response.message.text().is_empty());
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    }

    #[tokio::test]
    async fn a_tool_call_turn_returns_the_call() {
        let provider = provider(vec![ScriptedTurn::ToolCall {
            name: "read_file".into(),
            arguments: json!({ "path": "src/main.rs" }),
        }]);
        let (tx, rx) = unbounded_channel();

        let response = provider.stream(request(), tx).await.unwrap();
        let events = drain(rx).await;

        let call = ToolCall {
            id: "call_0".into(),
            name: "read_file".into(),
            arguments: json!({ "path": "src/main.rs" }),
        };
        assert_eq!(events[0], ProviderEvent::ToolCall(call.clone()));
        assert_eq!(response.tool_calls, vec![call.clone()]);
        assert_eq!(response.message.tool_calls, vec![call]);
        assert_eq!(response.message.text(), "");
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[tokio::test]
    async fn a_failing_turn_surfaces_as_a_provider_error() {
        let provider = provider(vec![ScriptedTurn::Fail("upstream exploded".into())]);
        let (tx, rx) = unbounded_channel();

        let err = provider.stream(request(), tx).await.unwrap_err();
        match err {
            HarnessError::Provider { provider, message } => {
                assert_eq!(provider, "mock");
                assert_eq!(message, "upstream exploded");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(drain(rx).await.is_empty());
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    async fn an_exhausted_script_ends_the_turn_instead_of_panicking() {
        let provider = provider(vec![ScriptedTurn::Text("only turn".into())]);
        let (tx, rx) = unbounded_channel();
        provider.stream(request(), tx).await.unwrap();
        let _ = drain(rx).await;

        let (tx, rx) = unbounded_channel();
        let response = provider.stream(request(), tx).await.unwrap();
        let events = drain(rx).await;

        assert_eq!(response.message.text(), "");
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
        assert_eq!(response.usage.output_tokens, 0);
        assert_eq!(
            events,
            vec![ProviderEvent::Usage(response.usage)],
            "an empty turn emits no deltas"
        );
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn one_scripted_turn_is_consumed_per_call() {
        let provider = provider(vec![
            ScriptedTurn::Text("first".into()),
            ScriptedTurn::Text("second".into()),
        ]);

        let (tx, rx) = unbounded_channel();
        let first = provider.stream(request(), tx).await.unwrap();
        let _ = drain(rx).await;
        let (tx, rx) = unbounded_channel();
        let second = provider.stream(request(), tx).await.unwrap();
        let _ = drain(rx).await;

        assert_eq!(first.message.text(), "first");
        assert_eq!(second.message.text(), "second");
        assert_eq!(provider.call_count(), 2);
    }

    #[test]
    fn deltas_respect_char_boundaries() {
        let pieces = split_deltas("设计一个后端架构并实现它吧");
        assert_eq!(pieces.concat(), "设计一个后端架构并实现它吧");
        assert!(pieces
            .iter()
            .all(|piece| piece.chars().count() <= MAX_DELTA_CHARS));
        assert!(pieces.len() > 1);
        assert!(split_deltas("").is_empty());
    }

    #[test]
    fn the_estimate_grows_with_the_input() {
        let estimator = HeuristicEstimator::default();
        let small = estimate_input(&estimator, &request());
        let large = estimate_input(
            &estimator,
            &ChatRequest::new(
                "mock-model",
                vec![
                    Message::user("hello there"),
                    Message::assistant("a much longer reply that costs more tokens"),
                ],
            ),
        );
        assert!(large > small);
    }

    #[tokio::test]
    async fn the_reasoning_effort_of_each_request_is_recorded() {
        let provider = provider(vec![
            ScriptedTurn::Text("a".into()),
            ScriptedTurn::Text("b".into()),
        ]);

        let mut with_effort = request();
        with_effort.reasoning_effort = Some("max".into());
        let (tx, rx) = unbounded_channel();
        provider.stream(with_effort, tx).await.unwrap();
        let _ = drain(rx).await;

        let (tx, rx) = unbounded_channel();
        provider.stream(request(), tx).await.unwrap();
        let _ = drain(rx).await;

        assert_eq!(
            provider.recorded_efforts(),
            vec![Some("max".to_string()), None]
        );
    }
}
