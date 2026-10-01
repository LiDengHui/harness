//! Planner / Executor / Verifier orchestration.
//!
//! A run has three stages. The planner turns a task into a [`graph::TaskGraph`]
//! of independent nodes; the executor runs each layer concurrently, isolating
//! nodes in git worktrees where it can; the verifier decides whether a node's
//! four-field [`result::SubtaskResult`] is acceptable, and a rejection feeds a
//! Reflexion retry rather than a silent pass.

pub mod executor;
pub mod graph;
pub mod planner;
pub mod result;
pub mod verify;
pub mod workflow;
pub mod worktree;

pub use executor::{ExecutionReport, Executor, ExecutorConfig, NodeOutcome, WorktreeRef};
pub use graph::{TaskGraph, TaskNode};
pub use planner::Planner;
pub use result::SubtaskResult;
pub use verify::{CheckOutcome, Issue, Severity, VerificationReport, Verifier, VerifierConfig};
pub use workflow::{SelectedWorkflow, WorkflowSelector};
pub use worktree::Worktree;
