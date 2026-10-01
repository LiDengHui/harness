//! The agent kernel: a ReAct loop over a provider, a tool registry and memory.
//!
//! Phase 1 ships the loop and its control channel. Phase 3 adds the declarative
//! layer: `AgentSpec` read from an `.agent.md` file, the `AgentRegistry` that
//! discovers them, the `ModelRouter` that picks a model, and the `TokenBudget`
//! plus `AgentRuntime` that fix what an agent may spend and touch.

pub mod agent_loop;
pub mod budget;
pub mod control;
mod guardrails;
pub mod prompt;
pub mod record;
pub mod registry;
pub mod router;
pub mod runtime;
pub mod spec;

pub use agent_loop::{AgentConfig, AgentLoop, RunOutcome};
pub use budget::{BudgetState, TokenBudget};
pub use control::{ControlChannel, ControlHandle, ControlMessage};
pub use prompt::Environment;
pub use record::{MemoryRecorder, TurnRecorder};
pub use registry::{search_dirs, AgentRegistry};
pub use router::{ModelRouter, ModelSource, ResolvedModel};
pub use runtime::AgentRuntime;
pub use spec::AgentSpec;
