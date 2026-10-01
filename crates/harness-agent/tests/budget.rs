//! Token budget enforcement: degradation at the threshold, hard stop at the ceiling.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use harness_agent::{AgentConfig, AgentLoop, ControlHandle, RunOutcome, TokenBudget};
use harness_core::{
    AgentEvent, AgentId, CompletionReason, HarnessError, Message, SessionId, TokenUsage, ToolCall,
    ToolsConfig,
};
use harness_llm::{ChatRequest, Provider, ProviderEvent, ProviderResponse};
use harness_tools::{ToolContext, ToolRegistry};
use serde_json::json;
use tokio::sync::mpsc;

/// Replays canned turns and records the model and tool count each request was
/// addressed to.
struct RecordingProvider {
    models: Mutex<Vec<String>>,
    tools: Mutex<Vec<usize>>,
    turns: Mutex<VecDeque<ProviderResponse>>,
}

impl RecordingProvider {
    fn new(turns: Vec<ProviderResponse>) -> Self {
        Self {
            models: Mutex::new(Vec::new()),
            tools: Mutex::new(Vec::new()),
            turns: Mutex::new(turns.into()),
        }
    }

    fn models(&self) -> Vec<String> {
        self.models.lock().unwrap().clone()
    }

    fn tools(&self) -> Vec<usize> {
        self.tools.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn model(&self) -> &str {
        "primary"
    }

    async fn stream(
        &self,
        request: ChatRequest,
        _events: mpsc::UnboundedSender<ProviderEvent>,
    ) -> harness_core::Result<ProviderResponse> {
        self.models.lock().unwrap().push(request.model.clone());
        self.tools.lock().unwrap().push(request.tools.len());
        self.turns
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| HarnessError::Provider {
                provider: "recording".into(),
                message: "script exhausted".into(),
            })
    }
}

/// A turn that asks for a tool. The tool does not have to exist: the loop turns
/// an unknown name into an error result, which is enough to keep the turn valid.
fn tool_turn(call_id: &str, input: u64, output: u64) -> ProviderResponse {
    let call = ToolCall {
        id: call_id.into(),
        name: "does_not_exist".into(),
        arguments: json!({}),
    };
    ProviderResponse {
        message: Message::assistant_tool_calls(vec![call.clone()]),
        tool_calls: vec![call],
        usage: TokenUsage::new(input, output),
        finish_reason: Some("tool_calls".into()),
    }
}

fn answer_turn(text: &str, input: u64, output: u64) -> ProviderResponse {
    ProviderResponse {
        message: Message::assistant(text),
        tool_calls: Vec::new(),
        usage: TokenUsage::new(input, output),
        finish_reason: Some("stop".into()),
    }
}

fn config(budget: Option<Arc<TokenBudget>>, fallback: Option<&str>) -> AgentConfig {
    let mut config = AgentConfig::new(
        AgentId::new("budgeted").unwrap(),
        SessionId::new(),
        "primary",
        "You are a test agent.",
    );
    config.fallback_model = fallback.map(str::to_string);
    config.budget = budget;
    config
}

async fn drive(
    provider: Arc<RecordingProvider>,
    config: AgentConfig,
) -> (RunOutcome, Vec<AgentEvent>) {
    let agent = AgentLoop::new(
        config,
        provider,
        ToolRegistry::with_builtins(),
        ToolContext::new(".", ToolsConfig::default()),
    );

    let (events, mut receiver) = mpsc::unbounded_channel();
    let (_handle, mut control) = ControlHandle::channel();
    let mut history = vec![Message::user("go")];

    let outcome = agent
        .run(&mut history, &events, &mut control)
        .await
        .unwrap();

    drop(events);
    let emitted = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
    (outcome, emitted)
}

fn remaining_reports(events: &[AgentEvent]) -> Vec<Option<u64>> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TokenUsage {
                budget_remaining, ..
            } => Some(*budget_remaining),
            _ => None,
        })
        .collect()
}

fn degradations(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, AgentEvent::Guardrail { name, .. } if name == "token_budget"),
        )
        .count()
}

#[tokio::test]
async fn crossing_the_threshold_switches_model_exactly_once() {
    let budget = Arc::new(TokenBudget::new(1_000));
    let provider = Arc::new(RecordingProvider::new(vec![
        tool_turn("call_1", 500, 350), // 850 spent, past the 80% mark
        answer_turn("done", 100, 10),  // 960 spent
    ]));

    let (outcome, events) = drive(
        Arc::clone(&provider),
        config(Some(Arc::clone(&budget)), Some("cheap-model")),
    )
    .await;

    assert_eq!(outcome.reason, CompletionReason::EndTurn);
    assert_eq!(provider.models(), vec!["primary", "cheap-model"]);
    assert_eq!(degradations(&events), 1, "degradation is announced once");
    assert_eq!(remaining_reports(&events), vec![Some(150), Some(40)]);
}

#[tokio::test]
async fn an_exhausted_budget_spends_its_last_turn_on_a_tool_free_wrap_up() {
    let budget = Arc::new(TokenBudget::new(1_000));
    let provider = Arc::new(RecordingProvider::new(vec![
        tool_turn("call_1", 900, 200), // 1_100 spent, past the ceiling
        answer_turn("the answer from what I already have", 10, 10),
    ]));

    let (outcome, events) = drive(Arc::clone(&provider), config(Some(budget), None)).await;

    assert_eq!(outcome.reason, CompletionReason::BudgetExceeded);
    assert_eq!(outcome.turns, 1, "the wrap-up is not a counted iteration");
    assert_eq!(
        outcome.final_text, "the answer from what I already have",
        "the run must answer from the work it already paid for"
    );

    // Two requests: the turn that crossed the ceiling, then the wrap-up. The
    // wrap-up advertises no tools, so it cannot start work the budget cannot pay
    // for and cannot leave a fresh tool call unanswered.
    let tools = provider.tools();
    assert_eq!(tools.len(), 2, "exactly one wrap-up turn");
    assert!(tools[0] > 0, "the normal turn advertises the tools");
    assert_eq!(tools[1], 0, "the wrap-up turn advertises none");

    // The stop happens at a boundary, so the turn's tool result is already in
    // the history and the conversation stays well formed.
    assert_eq!(remaining_reports(&events), vec![Some(0), Some(0)]);
}

#[tokio::test]
async fn without_a_budget_nothing_is_enforced() {
    let provider = Arc::new(RecordingProvider::new(vec![
        tool_turn("call_1", 100_000, 100_000),
        answer_turn("done", 100_000, 100_000),
    ]));

    let (outcome, events) = drive(Arc::clone(&provider), config(None, None)).await;

    assert_eq!(outcome.reason, CompletionReason::EndTurn);
    assert_eq!(degradations(&events), 0);
    assert!(remaining_reports(&events).iter().all(Option::is_none));
}

#[tokio::test]
async fn a_zero_budget_is_unbounded_rather_than_bricked() {
    let budget = Arc::new(TokenBudget::new(0));
    let provider = Arc::new(RecordingProvider::new(vec![
        tool_turn("call_1", 5_000_000, 5_000_000),
        answer_turn("done", 5_000_000, 5_000_000),
    ]));

    let (outcome, events) = drive(
        Arc::clone(&provider),
        config(Some(Arc::clone(&budget)), Some("cheap-model")),
    )
    .await;

    assert_eq!(outcome.reason, CompletionReason::EndTurn);
    assert_eq!(
        provider.models(),
        vec!["primary", "primary"],
        "an unbounded budget must never degrade"
    );
    assert_eq!(degradations(&events), 0);
}
