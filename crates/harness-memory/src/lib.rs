//! SQLite-backed DAG memory: snapshot, branch, trim, recall.
//!
//! The store keeps a conversation as a **DAG of immutable nodes** rather than a
//! flat log, which is what makes the three operations in `harness_core::Memory`
//! cheap:
//!
//! * `snapshot` pins a marker at the current head, so a later branch has a
//!   stable point to fork from.
//! * `branch` opens a new session that inherits the ancestor chain of a node in
//!   an existing session — see [`SqliteMemory::branch`] for why it inherits
//!   rather than copies.
//! * `trim` relieves context pressure *without losing information*: oversized
//!   tool output moves into the `blobs` table and the node keeps a reference to
//!   it, while every user, assistant and system turn stays byte-identical.
//!
//! Retrieval is hybrid: a lexical vector search fused with SQLite's FTS5 index
//! through reciprocal rank fusion, then cut down to the caller's token budget.
//! [`scenario`] shapes that query for the situation asking it, and [`stage`] adds
//! the labelling convention that lets a run be forked from a stage boundary
//! rather than only from its end.
//!
//! The embedding itself is [`HashingEmbedder`], a bag-of-words feature hash and
//! **not** a semantic model, so recall is best described as fuzzy keyword
//! matching; the FTS5 side is what keeps nodes without an embedding reachable.
//! The index is an exact in-memory cosine scan ([`BruteForceIndex`]) over
//! vectors read back from the `embeddings` table, so it stays correct across
//! restarts. `sqlite-vec` was evaluated as an ANN backend and dropped:
//! v0.1.10-alpha.4 fails to compile its C source on this toolchain, and exact
//! search is fast enough at the scale a session store actually reaches.

pub mod index;
pub mod recall;
pub mod scenario;
pub mod stage;
pub mod store;
pub mod trim;
pub mod vector;

pub use index::{reciprocal_rank_fusion, HybridIndex, RRF_K};
pub use recall::{fts_query, snippet, CANDIDATE_FANOUT, SNIPPET_CHARS};
pub use scenario::{recall_for, RecallBudget, RecallScenario};
pub use stage::{
    fork_from_stage, is_stage_label, node_stage, pin_stage, stage_label, stages, StageMarker,
};
pub use store::{SqliteMemory, BLOB_THRESHOLD_CHARS, DEFAULT_EMBEDDING_DIM};
pub use trim::{elide_tool_output, size_label, strip_inline_binary};
pub use vector::{BruteForceIndex, HashingEmbedder, VectorIndex};

mod hash;
