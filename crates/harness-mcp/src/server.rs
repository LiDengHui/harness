//! The client: connect, handshake, list tools, call tools.

use serde_json::{json, Value};

use harness_core::{McpServerConfig, Result, ToolSpec};

use crate::protocol::{
    notification, request, McpCallResult, McpServerInfo, McpToolSpec, CLIENT_NAME, PROTOCOL_VERSION,
};
use crate::transport::Transport;

/// A JSON-RPC 2.0 client for one MCP server.
///
/// Nothing leaves the process until the first request, and the tool list is
/// fetched only when something actually needs it. Schemas for a handful of
/// servers are a large slice of a model's context, and most runs never touch
/// most servers, so paying for them at startup is paying for nothing.
#[derive(Debug)]
pub struct McpServer {
    name: String,
    transport: Transport,
    next_id: u64,
    info: Option<McpServerInfo>,
    tools: Option<Vec<McpToolSpec>>,
}

impl McpServer {
    /// Builds a client for `config`.
    ///
    /// No process is launched and no request is sent here, so an unreachable or
    /// slow server cannot delay startup.
    pub async fn connect(name: impl Into<String>, config: &McpServerConfig) -> Result<Self> {
        Ok(Self {
            name: name.into(),
            transport: Transport::from_config(config)?,
            next_id: 1,
            info: None,
            tools: None,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// True once the handshake has completed and the transport is still usable.
    pub fn is_connected(&self) -> bool {
        self.info.is_some() && self.transport.is_open()
    }

    /// Performs the handshake once; later calls replay the cached negotiation.
    pub async fn initialize(&mut self) -> Result<McpServerInfo> {
        if let Some(info) = &self.info {
            return Ok(info.clone());
        }

        let id = self.take_id();
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": CLIENT_NAME,
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        let result = self
            .transport
            .request(&request(id, "initialize", params), id)
            .await?;
        let info = McpServerInfo::from_value(&result);

        // Best effort: a server that rejects the notification has still given us
        // a usable session, and failing the handshake over it would be wrong.
        if let Err(err) = self
            .transport
            .notify(&notification("notifications/initialized", json!({})))
            .await
        {
            tracing::debug!(
                "`{}` rejected the initialized notification: {err}",
                self.name
            );
        }

        self.info = Some(info.clone());
        Ok(info)
    }

    /// Fetches the tool list once and caches it.
    pub async fn tools(&mut self) -> Result<Vec<McpToolSpec>> {
        if let Some(tools) = &self.tools {
            return Ok(tools.clone());
        }

        self.initialize().await?;
        let id = self.take_id();
        let result = self
            .transport
            .request(&request(id, "tools/list", json!({})), id)
            .await?;
        let tools = McpToolSpec::list_from(&result)?;
        self.tools = Some(tools.clone());
        Ok(tools)
    }

    /// The advertised form of this server's tools, namespaced so they cannot
    /// shadow a builtin. Cheaper than building adapters when only the schemas
    /// are wanted.
    pub async fn tool_specs(&mut self) -> Result<Vec<ToolSpec>> {
        let name = self.name.clone();
        let tools = self.tools().await?;
        Ok(tools.iter().map(|spec| spec.to_tool_spec(&name)).collect())
    }

    pub async fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<McpCallResult> {
        self.initialize().await?;
        let id = self.take_id();
        let params = json!({ "name": tool, "arguments": arguments });
        let result = self
            .transport
            .request(&request(id, "tools/call", params), id)
            .await?;
        Ok(McpCallResult::from_value(result))
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.info = None;
        self.tools = None;
        self.transport.shutdown().await
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}
