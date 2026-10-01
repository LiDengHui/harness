//! Incremental persistence of a running conversation.
//!
//! The loop already mutates the caller's `history` in place, and callers used to
//! persist it only once the run returned. That is a single write point at the
//! end of a run, so an abort, a provider error or a killed process loses
//! everything the run produced — the user's question, the tool results, the
//! model's findings. A [`TurnRecorder`] moves the write to every turn boundary:
//! the loop hands it the messages appended since the previous call, so a crash
//! can cost at most the turn in flight.
//!
//! The trait is deliberately not tied to `harness-memory`: it speaks the
//! `harness_core::Memory` contract, so a caller can back it with SQLite, a
//! remote store or a test double without the loop knowing.

use std::sync::Arc;

use async_trait::async_trait;
use harness_core::{Memory, Message, SessionId};

/// Receives the conversation as the loop produces it.
///
/// `record` is called at turn boundaries with the messages appended since the
/// last call, in order. It returns how many of them it made durable; the loop
/// advances its cursor by that many, so a partial or failed write is retried on
/// the next turn rather than being dropped or appended twice.
#[async_trait]
pub trait TurnRecorder: Send + Sync {
    async fn record(&self, messages: &[Message]) -> usize;
}

/// A [`TurnRecorder`] that appends to a `Memory` session.
///
/// A failed append stops the batch and is logged, not propagated: the run's
/// verdict does not depend on its trace, and a store that has gone away must not
/// turn a successful answer into a failed run. The unwritten messages stay
/// pending, so the next turn boundary tries them again.
pub struct MemoryRecorder {
    memory: Arc<dyn Memory>,
    session_id: SessionId,
}

impl MemoryRecorder {
    pub fn new(memory: Arc<dyn Memory>, session_id: SessionId) -> Self {
        Self { memory, session_id }
    }

    /// The session every recorded turn is appended to.
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}

#[async_trait]
impl TurnRecorder for MemoryRecorder {
    async fn record(&self, messages: &[Message]) -> usize {
        let mut written = 0usize;
        for message in messages {
            match self.memory.append(self.session_id, message).await {
                Ok(_) => written += 1,
                Err(err) => {
                    tracing::warn!(
                        "incremental persistence stopped after {written}/{} turns: {err}",
                        messages.len()
                    );
                    break;
                }
            }
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use harness_core::{
        HarnessError, MemoryStats, Node, NodeId, RecallHit, RecallQuery, SessionInfo, TrimOptions,
        TrimReport,
    };
    use std::sync::Mutex;

    /// Records every append it is asked to make, and can fail on demand.
    struct SpyMemory {
        appended: Mutex<Vec<(SessionId, Message)>>,
        fail_at: Option<usize>,
    }

    #[async_trait]
    impl Memory for SpyMemory {
        async fn create_session(&self, _label: Option<&str>) -> harness_core::Result<SessionId> {
            Ok(SessionId::new())
        }
        async fn sessions(&self) -> harness_core::Result<Vec<SessionInfo>> {
            Ok(Vec::new())
        }
        async fn session(&self, _id: SessionId) -> harness_core::Result<Option<SessionInfo>> {
            Ok(None)
        }
        async fn head(&self, _id: SessionId) -> harness_core::Result<Option<NodeId>> {
            Ok(None)
        }
        async fn append(
            &self,
            session_id: SessionId,
            message: &Message,
        ) -> harness_core::Result<NodeId> {
            let mut appended = self.appended.lock().unwrap();
            if self.fail_at == Some(appended.len()) {
                return Err(HarnessError::Memory("the store is unavailable".into()));
            }
            appended.push((session_id, message.clone()));
            Ok(NodeId::new())
        }
        async fn snapshot(&self, _id: SessionId, _label: &str) -> harness_core::Result<NodeId> {
            Ok(NodeId::new())
        }
        async fn branch(
            &self,
            _from_session: SessionId,
            _from_node: NodeId,
            _label: Option<&str>,
        ) -> harness_core::Result<SessionId> {
            Ok(SessionId::new())
        }
        async fn history(
            &self,
            _id: SessionId,
            _head: Option<NodeId>,
        ) -> harness_core::Result<Vec<Message>> {
            Ok(Vec::new())
        }
        async fn path(&self, _id: NodeId) -> harness_core::Result<Vec<Node>> {
            Ok(Vec::new())
        }
        async fn node(&self, _id: NodeId) -> harness_core::Result<Option<Node>> {
            Ok(None)
        }
        async fn children(&self, _parent: NodeId) -> harness_core::Result<Vec<Node>> {
            Ok(Vec::new())
        }
        async fn trim(&self, _id: SessionId, _o: TrimOptions) -> harness_core::Result<TrimReport> {
            Ok(TrimReport::default())
        }
        async fn recall(&self, _query: RecallQuery) -> harness_core::Result<Vec<RecallHit>> {
            Ok(Vec::new())
        }
        async fn raw_output(&self, _id: NodeId) -> harness_core::Result<Option<String>> {
            Ok(None)
        }
        async fn stats(&self) -> harness_core::Result<MemoryStats> {
            Ok(MemoryStats::default())
        }
    }

    #[tokio::test]
    async fn every_message_is_appended_in_order() {
        let memory = Arc::new(SpyMemory {
            appended: Mutex::new(Vec::new()),
            fail_at: None,
        });
        let session = SessionId::new();
        let recorder = MemoryRecorder::new(memory.clone(), session);

        let messages = vec![Message::user("one"), Message::assistant("two")];
        assert_eq!(recorder.record(&messages).await, 2);

        let appended = memory.appended.lock().unwrap();
        assert_eq!(appended.len(), 2);
        assert_eq!(appended[0].0, session);
        assert_eq!(appended[0].1.text(), "one");
        assert_eq!(appended[1].1.text(), "two");
    }

    #[tokio::test]
    async fn a_failed_append_reports_how_far_it_got() {
        let memory = Arc::new(SpyMemory {
            appended: Mutex::new(Vec::new()),
            fail_at: Some(1),
        });
        let recorder = MemoryRecorder::new(memory.clone(), SessionId::new());

        let messages = vec![
            Message::user("one"),
            Message::assistant("two"),
            Message::assistant("three"),
        ];
        // The first append lands, the second fails, so the loop keeps "two" and
        // "three" pending for the next turn instead of losing or duplicating them.
        assert_eq!(recorder.record(&messages).await, 1);
        assert_eq!(memory.appended.lock().unwrap().len(), 1);
    }
}
