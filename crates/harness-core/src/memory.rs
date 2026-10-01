//! The memory contract.
//!
//! Memory is modelled as a **DAG of immutable nodes**, not a linear log. Three
//! primitives operate on it:
//!
//! * `snapshot` — pins a versioned marker at a head, so a later `branch` has a
//!   stable point to fork from.
//! * `branch` — forks a new session that inherits an ancestor chain, which is how
//!   parallel sub-agents share history without copying it.
//! * `trim` — *lossless* pressure relief. It rewrites tool turns into compact
//!   references while leaving every user and assistant turn byte-identical; the
//!   full tool output stays retrievable through [`Memory::raw_output`].
//!
//! The contract lives in `harness-core` so that the storage implementation and
//! the agent kernel can be developed and tested independently.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, Result};
use crate::id::{NodeId, SessionId};
use crate::message::{Message, Role};

/// What a node represents in the DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A conversational turn.
    Turn,
    /// A versioned marker recording the head it was pinned at.
    Snapshot,
}

/// One immutable node of the conversation DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub session_id: SessionId,
    pub parent_id: Option<NodeId>,
    pub kind: NodeKind,
    pub role: Option<Role>,
    /// The text as it was (or would be) sent to the model. Never rewritten by
    /// `trim` for conversational roles.
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<crate::message::ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional human label, used by snapshot and branch markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Estimated token cost of `content`.
    pub tokens: usize,
    /// True once `trim` has replaced this node's tool output with a reference.
    #[serde(default)]
    pub trimmed: bool,
    pub created_at: DateTime<Utc>,
}

impl Node {
    /// Converts the node back into the message the provider expects.
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role.unwrap_or(Role::User),
            content: if self.content.is_empty() {
                None
            } else {
                Some(self.content.clone())
            },
            tool_calls: self.tool_calls.clone(),
            tool_call_id: self.tool_call_id.clone(),
            name: self.label.clone(),
        }
    }
}

/// Summary of one stored session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub label: Option<String>,
    /// Human-facing preview of the session's first user turn, so a session list
    /// can tell one run from another without opening it. `None` until the user
    /// has actually said something.
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Node rows this session owns. A forked session shares its ancestor chain
    /// by reference rather than copying it, so this is smaller than the session's
    /// history length. Use `Memory::history` for the effective turn count.
    pub nodes: usize,
    /// Estimated tokens across the rows this session owns.
    pub tokens: usize,
    pub head: Option<NodeId>,
    /// Set when this session was forked from another node.
    pub forked_from: Option<NodeId>,
    /// The session that owns the node named by `forked_from`.
    ///
    /// `forked_from` says where in the DAG a fork was taken, but not which
    /// conversation to group it under: a node id is not a session id. The UI
    /// draws the session tree from this instead — a session's children are the
    /// sessions whose `forked_from_session` is its id. `None` for a session that
    /// is not a fork, and also `None` when the referenced node cannot be
    /// resolved, so a dangling reference cannot make a session unlistable.
    pub forked_from_session: Option<SessionId>,
}

/// Knobs for [`Memory::trim`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrimOptions {
    /// Most recent turns are left untouched so the active context keeps detail.
    pub keep_recent: usize,
    /// Tool outputs shorter than this are not worth replacing with a reference.
    pub min_tool_output_chars: usize,
    /// Also elide base64 data URIs found inside assistant turns.
    pub strip_base64: bool,
}

impl Default for TrimOptions {
    fn default() -> Self {
        Self {
            keep_recent: 2,
            min_tool_output_chars: 512,
            strip_base64: true,
        }
    }
}

/// What a `trim` pass achieved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TrimReport {
    pub nodes_examined: usize,
    pub nodes_trimmed: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

impl TrimReport {
    pub fn saved_tokens(&self) -> usize {
        self.tokens_before.saturating_sub(self.tokens_after)
    }

    pub fn saved_percent(&self) -> f64 {
        if self.tokens_before == 0 {
            return 0.0;
        }
        self.saved_tokens() as f64 * 100.0 / self.tokens_before as f64
    }
}

/// A hybrid recall request: vector similarity fused with full-text search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallQuery {
    /// Restrict to one session; `None` searches across every session.
    pub session_id: Option<SessionId>,
    pub text: String,
    pub limit: usize,
    /// Hard ceiling on the tokens the hits may occupy once injected.
    pub max_tokens: usize,
    /// Restrict to these roles; empty means every role.
    pub roles: Vec<Role>,
}

impl RecallQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            session_id: None,
            text: text.into(),
            limit: 8,
            max_tokens: 2_000,
            roles: Vec::new(),
        }
    }
}

/// One retrieved node, already sized against the caller's token budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallHit {
    pub node_id: NodeId,
    pub session_id: SessionId,
    pub role: Option<Role>,
    pub snippet: String,
    /// Fused score; higher is better. Only comparable within one result set.
    pub score: f32,
    pub tokens: usize,
    /// True when the snippet is a prefix of a longer node body.
    pub truncated: bool,
}

/// What a [`Memory::delete_session`] removed.
///
/// Counts rather than a boolean because a delete is destructive and a caller
/// should be able to say what went, not only that something did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDeleteReport {
    /// Node rows deleted from the session.
    pub nodes: usize,
    /// Blob rows deleted. Includes rows whose body was already compacted away
    /// by a trim: the row still names the deleted node, so it goes with it.
    pub blobs: usize,
    /// Embedding rows deleted.
    pub embeddings: usize,
    /// Sessions forked from this one that were **left in place**; see
    /// [`Memory::delete_session`] for why they are not cascaded.
    pub forks: usize,
}

/// Storage statistics, used by `harness memory stats` and the token report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryStats {
    pub sessions: usize,
    pub nodes: usize,
    /// Raw tool outputs kept out of the context but still retrievable.
    pub blobs: usize,
    pub blob_bytes: usize,
    pub total_tokens: usize,
}

/// The storage contract used by the agent kernel.
#[async_trait]
pub trait Memory: Send + Sync {
    async fn create_session(&self, label: Option<&str>) -> Result<SessionId>;

    async fn sessions(&self) -> Result<Vec<SessionInfo>>;

    async fn session(&self, session_id: SessionId) -> Result<Option<SessionInfo>>;

    /// The node a new turn would be appended to.
    async fn head(&self, session_id: SessionId) -> Result<Option<NodeId>>;

    /// Appends a turn onto the session head, returning the new node id.
    async fn append(&self, session_id: SessionId, message: &Message) -> Result<NodeId>;

    /// Pins a versioned marker at the current head and returns the marker node.
    async fn snapshot(&self, session_id: SessionId, label: &str) -> Result<NodeId>;

    /// Forks `from_node` (and its ancestors) into a brand new session.
    async fn branch(
        &self,
        from_session: SessionId,
        from_node: NodeId,
        label: Option<&str>,
    ) -> Result<SessionId>;

    /// Reconstructs the message list leading to `head` (or the session head).
    async fn history(&self, session_id: SessionId, head: Option<NodeId>) -> Result<Vec<Message>>;

    /// Every node on the path from the root to `head`, inclusive.
    async fn path(&self, node_id: NodeId) -> Result<Vec<Node>>;

    async fn node(&self, node_id: NodeId) -> Result<Option<Node>>;

    /// Nodes that name `parent` as their parent. More than one means a fork.
    async fn children(&self, parent: NodeId) -> Result<Vec<Node>>;

    /// Lossless pressure relief; see the module docs.
    async fn trim(&self, session_id: SessionId, options: TrimOptions) -> Result<TrimReport>;

    /// Hybrid vector + keyword retrieval, constrained by a token budget.
    async fn recall(&self, query: RecallQuery) -> Result<Vec<RecallHit>>;

    /// The full, untrimmed output behind a node, if one was stored.
    async fn raw_output(&self, node_id: NodeId) -> Result<Option<String>>;

    async fn stats(&self) -> Result<MemoryStats>;

    /// Deletes a session and the rows it owns: its nodes (and their FTS
    /// entries), the blobs and embeddings those nodes own, and the session row
    /// itself.
    ///
    /// Returns `Ok(None)` when `session_id` names no session, so a caller can
    /// tell "already gone" from "deleted"; a backend that does not implement
    /// deletion returns an error instead of pretending it removed something.
    ///
    /// **Forks are not cascaded.** A session forked from this one is a separate
    /// conversation with its own nodes; deleting it silently would destroy work
    /// the caller did not name. What such a fork does lose is the ancestor chain
    /// it inherited, because those nodes belonged to the deleted session: after
    /// this call its history starts at its own fork marker. The report's `forks`
    /// count says how many sessions were left in that state so a caller can
    /// warn.
    async fn delete_session(&self, session_id: SessionId) -> Result<Option<SessionDeleteReport>> {
        let _ = session_id;
        Err(HarnessError::Memory(
            "this memory backend cannot delete sessions".to_string(),
        ))
    }
}
