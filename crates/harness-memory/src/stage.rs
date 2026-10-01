//! Stage boundaries: pinning a snapshot at each boundary of a run.
//!
//! The DAG already supports `snapshot`, but nothing named the boundaries of a
//! run, so the only fork point anyone could find was the head — the end of the
//! work. That makes a run a single blob: a later branch can only continue from
//! the finish, never re-run one stage or inspect what a stage had produced when
//! it ended.
//!
//! This module is the thin convention on top of `Memory::snapshot`:
//!
//! * [`pin_stage`] pins a snapshot at the session head and returns a
//!   [`StageMarker`] naming it.
//! * [`stages`] lists a session's own stage markers, oldest first, so a caller
//!   can pick the boundary it wants.
//! * [`fork_from_stage`] branches from a marker, which is how a run is replayed
//!   from a stage rather than from its end.
//!
//! Stage labels are prefixed so a fork marker (whose label is a node id) is not
//! mistaken for a stage. The prefix is part of the public contract:
//! [`stage_label`] builds it and [`is_stage_label`] recognises it.

use chrono::{DateTime, Utc};
use harness_core::{HarnessError, Memory, Node, NodeId, NodeKind, Result, SessionId};

/// Prefix that distinguishes a stage marker from a branch's fork marker.
const STAGE_PREFIX: &str = "stage:";

/// The label a stage boundary carries.
pub fn stage_label(stage: &str) -> String {
    format!("{STAGE_PREFIX}{stage}")
}

/// True when `label` names a stage boundary rather than a fork.
pub fn is_stage_label(label: &str) -> bool {
    label.starts_with(STAGE_PREFIX)
}

/// The stage name for one boundary of a DAG node: `node:<node id>:<boundary>`.
///
/// The node id is in the label so a stage can be traced back to the node that
/// produced it, and so two nodes' `start` boundaries cannot collide.
pub fn node_stage(node_id: &str, boundary: &str) -> String {
    format!("node:{node_id}:{boundary}")
}

/// A pinned snapshot a later branch can fork from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageMarker {
    pub node_id: NodeId,
    pub session_id: SessionId,
    pub label: String,
    pub created_at: DateTime<Utc>,
}

impl StageMarker {
    /// Forks a new session from this stage.
    ///
    /// The branch inherits the marker's ancestor chain, so it sees exactly the
    /// state the run had reached at this boundary — not the run's final state.
    pub async fn fork(&self, memory: &dyn Memory, label: Option<&str>) -> Result<SessionId> {
        fork_from_stage(memory, self, label).await
    }

    fn from_node(node: &Node) -> Self {
        Self {
            node_id: node.id,
            session_id: node.session_id,
            label: node.label.clone().unwrap_or_default(),
            created_at: node.created_at,
        }
    }
}

/// Pins a stage boundary at `session_id`'s current head.
///
/// Pinning at the head is what makes the marker a boundary: everything the
/// session has written so far is behind it, nothing after it exists yet.
pub async fn pin_stage(
    memory: &dyn Memory,
    session_id: SessionId,
    stage: &str,
) -> Result<StageMarker> {
    let label = stage_label(stage);
    let node_id = memory.snapshot(session_id, &label).await?;
    let node = memory.node(node_id).await?.ok_or_else(|| {
        HarnessError::Memory(format!(
            "stage marker {node_id} vanished after it was pinned"
        ))
    })?;
    Ok(StageMarker::from_node(&node))
}

/// Every stage marker `session_id` pinned, oldest first.
///
/// Only markers this session owns are returned. A forked session's ancestor
/// chain carries the parent's stages too, and those are not boundaries of this
/// session's run.
pub async fn stages(memory: &dyn Memory, session_id: SessionId) -> Result<Vec<StageMarker>> {
    let Some(head) = memory.head(session_id).await? else {
        return Ok(Vec::new());
    };

    // `path` is root-first, so the markers come back in the order they were
    // pinned.
    Ok(memory
        .path(head)
        .await?
        .into_iter()
        .filter(|node| {
            node.session_id == session_id
                && node.kind == NodeKind::Snapshot
                && node.label.as_deref().is_some_and(is_stage_label)
        })
        .map(|node| StageMarker::from_node(&node))
        .collect())
}

/// Forks a new session from a stage marker.
pub async fn fork_from_stage(
    memory: &dyn Memory,
    stage: &StageMarker,
    label: Option<&str>,
) -> Result<SessionId> {
    memory.branch(stage.session_id, stage.node_id, label).await
}
