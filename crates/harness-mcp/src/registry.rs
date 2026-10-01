//! Wiring configured servers into the local tool registry.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use harness_core::{McpConfig, McpServerConfig, Result};
use harness_tools::ToolRegistry;

use crate::adapter::McpToolAdapter;
use crate::server::McpServer;

/// Every MCP server the harness is configured with, connected and ready.
///
/// A server that fails is recorded in [`McpRegistry::failures`] instead of
/// failing the whole startup: one broken third-party server must not cost the
/// run its filesystem and shell tools.
pub struct McpRegistry {
    tools: ToolRegistry,
    servers: BTreeMap<String, Arc<Mutex<McpServer>>>,
    failures: Vec<String>,
}

impl McpRegistry {
    /// Connects the servers that are not marked `lazy`.
    pub async fn connect(config: &McpConfig) -> Self {
        Self::connect_all(config, false).await
    }

    /// `include_lazy` forces every enabled server to be connected now, which is
    /// what a caller wants when it is about to advertise the full tool list.
    pub async fn connect_all(config: &McpConfig, include_lazy: bool) -> Self {
        let mut registry = Self {
            tools: ToolRegistry::new(),
            servers: BTreeMap::new(),
            failures: Vec::new(),
        };

        for (name, server_config) in config.enabled() {
            if server_config.lazy && !include_lazy {
                tracing::debug!("deferring mcp server `{name}`");
                continue;
            }
            if let Err(err) = registry.add(name, server_config).await {
                tracing::warn!("mcp server `{name}` is unavailable: {err}");
                registry.failures.push(format!("{name}: {err}"));
            }
        }
        registry
    }

    /// Connects one server and registers an adapter per tool it exposes.
    ///
    /// This is also the entry point for activating a `lazy` server later.
    /// Returns the local names that were registered.
    pub async fn add(&mut self, name: &str, config: &McpServerConfig) -> Result<Vec<String>> {
        let handle = Arc::new(Mutex::new(McpServer::connect(name, config).await?));

        // Scoped so the guard is released before adapters are built: they read
        // the server's name without waiting on it.
        let specs = {
            let mut server = handle.lock().await;
            server.tools().await?
        };

        let mut local_names = Vec::new();
        for spec in specs {
            let adapter = McpToolAdapter::new(handle.clone(), spec);
            local_names.push(adapter.local_name().to_string());
            self.tools.register(Arc::new(adapter));
        }

        self.servers.insert(name.to_string(), handle);
        Ok(local_names)
    }

    /// The adapters, ready to be merged into the builtin registry.
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    pub fn into_tools(self) -> ToolRegistry {
        self.tools
    }

    /// Names of the servers that connected.
    pub fn names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    pub fn server(&self, name: &str) -> Option<Arc<Mutex<McpServer>>> {
        self.servers.get(name).cloned()
    }

    /// One entry per configured server that could not be connected.
    pub fn failures(&self) -> &[String] {
        &self.failures
    }

    pub async fn shutdown(&mut self) {
        for (name, server) in &self.servers {
            if let Err(err) = server.lock().await.shutdown().await {
                tracing::debug!("shutting down mcp server `{name}` failed: {err}");
            }
        }
        self.servers.clear();
    }
}
