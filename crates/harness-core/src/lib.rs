//! Core types shared by every harness crate.
//!
//! This crate is the dependency floor of the workspace: it must not depend on
//! any other harness crate.

pub mod config;
pub mod error;
pub mod event;
pub mod frontmatter;
pub mod id;
pub mod memory;
pub mod message;
pub mod path;
pub mod permission;
pub mod tokens;
pub mod tool;

pub use config::{
    global_dir, project_dir, resolve_thinking_effort, Config, GuardrailsConfig, HarnessSection,
    McpConfig, McpServerConfig, McpTransportKind, MemoryConfig, PermissionsConfig, ProviderConfig,
    ProviderKind, ProviderSection, ServerConfig, ThinkingConfig, ToolsConfig, UiConfig,
    DEFAULT_PERMISSION_TIMEOUT_SECS, DEFAULT_THINKING_EFFORT, HARNESS_DIR,
    SUPPORTED_THINKING_EFFORTS, SUPPORTED_UI_LANGUAGES,
};
pub use error::{HarnessError, Result};
pub use event::{
    AgentEvent, ClientEnvelope, ClientMessage, CompletionReason, PlanNode, Priority, RoutedEvent,
    ServerEnvelope, ServerMessage, SubtaskStatus, ToolCallStatus, PROTOCOL_VERSION,
};
pub use id::{AgentId, MessageId, NodeId, SessionId, SubtaskId};
pub use memory::{
    Memory, MemoryStats, Node, NodeKind, RecallHit, RecallQuery, SessionDeleteReport, SessionInfo,
    TrimOptions, TrimReport,
};
pub use message::{Message, Role, ToolCall};
pub use permission::{classify, mode_requires_approval, PermissionMode, ToolRisk};
pub use tokens::{HeuristicEstimator, TokenEstimator, TokenUsage};
pub use tool::{object_schema, ToolSpec};
