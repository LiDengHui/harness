//! The loop's incremental persistence: a run that fails or is aborted keeps
//! every turn it had already completed.

use std::sync::Arc;
use std::time::Duration;

use harness_agent::{AgentConfig, AgentLoop, ControlHandle, MemoryRecorder};
use harness_core::{AgentId, Memory, Message, Role, SessionId, ToolsConfig};
use harness_llm::{MockProvider, ScriptedTurn};
use harness_memory::SqliteMemory;
use harness_tools::{ToolContext, ToolRegistry};
use serde_json::json;
use tokio::sync::mpsc;

fn config(session: SessionId) -> AgentConfig {
    AgentConfig::new(
        AgentId::new("tester").unwrap(),
        session,
        "mock-1",
        "You are a test agent.",
    )
}

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(root, ToolsConfig::default())
}

/// Returns one scripted tool-call turn, then fails — the shape of a provider
/// that dies between turns. `MockProvider` deliberately treats an exhausted
/// script as a normal end of conversation, so it cannot express this.
struct FailsAfterFirstTurn;

#[async_trait::async_trait]
impl harness_llm::Provider for FailsAfterFirstTurn {
    fn id(&self) -> &str {
        "failing"
    }

    fn model(&self) -> &str {
        "failing-1"
    }

    async fn stream(
        &self,
        _request: harness_llm::ChatRequest,
        _events: mpsc::UnboundedSender<harness_llm::ProviderEvent>,
    ) -> harness_core::Result<harness_llm::ProviderResponse> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        if CALLS.fetch_add(1, Ordering::SeqCst) > 0 {
            return Err(harness_core::HarnessError::Provider {
                provider: "failing".into(),
                message: "the provider went away".into(),
            });
        }
        let call = harness_core::ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: json!({ "path": "note.txt" }),
        };
        Ok(harness_llm::ProviderResponse {
            message: Message::assistant_tool_calls(vec![call.clone()]),
            tool_calls: vec![call],
            usage: harness_core::TokenUsage::new(10, 5),
            finish_reason: Some("tool_calls".into()),
        })
    }
}

#[tokio::test]
async fn a_run_that_fails_mid_way_keeps_every_completed_turn() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("note.txt"), "hello from the file").unwrap();

    let memory = Arc::new(SqliteMemory::in_memory().await.unwrap());
    let session = memory.create_session(Some("crashy")).await.unwrap();
    let recorder = MemoryRecorder::new(memory.clone(), session);
    let agent = AgentLoop::new(
        config(session),
        Arc::new(FailsAfterFirstTurn),
        ToolRegistry::with_builtins(),
        context(tmp.path()),
    );

    let (events, _rx) = mpsc::unbounded_channel();
    let (_handle, mut control) = ControlHandle::channel();
    let mut history = vec![Message::user("read note.txt")];

    let err = agent
        .run_persisting(&mut history, &events, &mut control, &recorder, 0)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("went away"), "{err}");

    let stored = memory.history(session, None).await.unwrap();
    assert_eq!(
        stored.len(),
        3,
        "the completed turn must be durable: {:?}",
        stored
            .iter()
            .map(|message| message.text())
            .collect::<Vec<_>>()
    );
    assert_eq!(stored[0].text(), "read note.txt");
    assert_eq!(stored[1].role, Role::Assistant);
    assert_eq!(stored[1].tool_calls.len(), 1);
    assert_eq!(stored[2].role, Role::Tool);
    assert!(stored[2].text().contains("hello from the file"));
}

#[tokio::test]
async fn an_aborted_run_keeps_the_turn_in_flight_but_not_the_next_one() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::new(
        "mock",
        "mock-1",
        vec![
            ScriptedTurn::ToolCall {
                name: "shell".into(),
                arguments: json!({ "command": "sleep 30" }),
            },
            ScriptedTurn::Text("should never be reached".into()),
        ],
    ));

    let memory = Arc::new(SqliteMemory::in_memory().await.unwrap());
    let session = memory.create_session(Some("aborted")).await.unwrap();
    let recorder = MemoryRecorder::new(memory.clone(), session);
    let agent = AgentLoop::new(
        config(session),
        provider,
        ToolRegistry::with_builtins(),
        context(tmp.path()),
    );

    let (events, _rx) = mpsc::unbounded_channel();
    let (handle, mut control) = ControlHandle::channel();
    let mut history = vec![Message::user("go")];

    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let trigger = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            handle.abort(Some("stop".into()))
        });
        let outcome = agent
            .run_persisting(&mut history, &events, &mut control, &recorder, 0)
            .await
            .unwrap();
        let _ = trigger.await;
        outcome
    })
    .await
    .expect("the run ignored the abort");

    assert_eq!(outcome.reason, harness_core::CompletionReason::Aborted);

    let stored = memory.history(session, None).await.unwrap();
    assert_eq!(
        stored.len(),
        3,
        "the aborted turn is durable: {:?}",
        stored
            .iter()
            .map(|message| message.text())
            .collect::<Vec<_>>()
    );
    assert!(
        stored
            .iter()
            .all(|message| !message.text().contains("should never be reached")),
        "a turn that never ran must not be persisted"
    );
    // The aborted call is still answered, so the stored conversation is sendable.
    let tool = stored
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("the aborted call is answered");
    assert!(tool.text().contains("aborted"), "{}", tool.text());
}

#[tokio::test]
async fn the_recorded_history_matches_the_runs_own_history() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("note.txt"), "content").unwrap();
    let provider = Arc::new(MockProvider::new(
        "mock",
        "mock-1",
        vec![
            ScriptedTurn::ToolCall {
                name: "read_file".into(),
                arguments: json!({ "path": "note.txt" }),
            },
            ScriptedTurn::Text("done".into()),
        ],
    ));

    let memory = Arc::new(SqliteMemory::in_memory().await.unwrap());
    let session = memory.create_session(Some("complete")).await.unwrap();
    let recorder = MemoryRecorder::new(memory.clone(), session);
    let agent = AgentLoop::new(
        config(session),
        provider,
        ToolRegistry::with_builtins(),
        context(tmp.path()),
    );

    let (events, _rx) = mpsc::unbounded_channel();
    let (_handle, mut control) = ControlHandle::channel();
    let mut history = vec![Message::user("read note.txt")];

    agent
        .run_persisting(&mut history, &events, &mut control, &recorder, 0)
        .await
        .unwrap();

    let stored = memory.history(session, None).await.unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|message| message.text())
            .collect::<Vec<_>>(),
        history
            .iter()
            .map(|message| message.text())
            .collect::<Vec<_>>(),
        "a completed run must be persisted exactly once, in order"
    );
}
