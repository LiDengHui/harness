//! The two wires an MCP client speaks: a child process's pipes, or HTTP POSTs.

mod http;
mod stdio;

use serde_json::Value;

use harness_core::{McpServerConfig, McpTransportKind, Result};

use http::HttpTransport;
use stdio::StdioTransport;

/// Upper bound on a single protocol message.
///
/// Enforced while reading, not after, so a server that never terminates a line
/// cannot grow the client's buffer without bound.
pub(crate) const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum Transport {
    Stdio(Box<StdioTransport>),
    Http(HttpTransport),
}

impl Transport {
    pub(crate) fn from_config(config: &McpServerConfig) -> Result<Self> {
        match config.transport_kind()? {
            McpTransportKind::Stdio => {
                let command = config
                    .command
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                Ok(Self::Stdio(Box::new(StdioTransport::new(
                    command,
                    config.args.clone(),
                    config.env.clone(),
                ))))
            }
            McpTransportKind::Http => {
                let url = config.url.as_deref().unwrap_or_default().trim();
                Ok(Self::Http(HttpTransport::new(url)?))
            }
        }
    }

    /// Sends `message` and returns the `result` of the response carrying `id`.
    pub(crate) async fn request(&mut self, message: &Value, id: u64) -> Result<Value> {
        match self {
            Self::Stdio(transport) => transport.request(message, id).await,
            Self::Http(transport) => transport.request(message, id).await,
        }
    }

    /// Fire and forget; the caller decides whether a failure matters.
    pub(crate) async fn notify(&mut self, message: &Value) -> Result<()> {
        match self {
            Self::Stdio(transport) => transport.notify(message).await,
            Self::Http(transport) => transport.notify(message).await,
        }
    }

    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        match self {
            Self::Stdio(transport) => transport.shutdown().await,
            Self::Http(transport) => transport.shutdown().await,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        match self {
            Self::Stdio(transport) => transport.is_open(),
            Self::Http(transport) => transport.is_open(),
        }
    }
}
