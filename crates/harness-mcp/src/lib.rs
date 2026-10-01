//! Model Context Protocol client (stdio + streamable HTTP)
//!
//! A remote MCP tool is wrapped as an ordinary [`harness_tools::Tool`] named
//! `mcp__<server>__<tool>`, so MCP servers plug into the same registry as the
//! builtins and cannot shadow them.
//!
//! # Deferred schemas
//!
//! [`McpServer::connect`] performs no I/O at all. The handshake happens on the
//! first request, and the tool list only when [`McpServer::tools`] or a tool
//! call needs it. Tool schemas are a large and mostly unused slice of a model's
//! context, so a configured-but-idle server should cost nothing; [`McpRegistry`]
//! keeps that window open by skipping servers marked `lazy`.

mod adapter;
mod protocol;
mod registry;
mod server;
mod transport;

pub use adapter::McpToolAdapter;
pub use protocol::{
    qualified_name, McpCallResult, McpServerInfo, McpToolSpec, PROTOCOL_VERSION, TOOL_PREFIX,
};
pub use registry::McpRegistry;
pub use server::McpServer;
