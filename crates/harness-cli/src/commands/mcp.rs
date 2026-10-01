//! `harness mcp`: inspect configured MCP servers and call their tools.
//!
//! Server definitions live in `[mcp.servers.<name>]`. Nothing connects until a
//! command asks for it, which is the same deferred-schema policy the agent uses:
//! an unconfigured or unused server costs nothing.

use harness_core::McpTransportKind;
use harness_mcp::{McpRegistry, McpServer};

use crate::commands::McpCommand;
use crate::context::AppContext;

pub(crate) async fn execute(command: McpCommand, ctx: &AppContext) -> anyhow::Result<()> {
    match command {
        McpCommand::List => list(ctx).await,
        McpCommand::Call { server, tool, json } => call(ctx, &server, &tool, &json).await,
    }
}

async fn list(ctx: &AppContext) -> anyhow::Result<()> {
    if ctx.config.mcp.servers.is_empty() {
        println!("no MCP servers configured");
        println!("add [mcp.servers.<name>] to .harness/config.toml with either `command` or `url`");
        return Ok(());
    }

    println!(
        "{:<18} {:<7} {:<8} {:<6} target",
        "server", "kind", "enabled", "lazy"
    );
    for (name, config) in &ctx.config.mcp.servers {
        let kind = match config.transport_kind() {
            Ok(McpTransportKind::Stdio) => "stdio",
            Ok(McpTransportKind::Http) => "http",
            Err(_) => "invalid",
        };
        let target = config
            .url
            .clone()
            .or_else(|| config.command.clone())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{name:<18} {kind:<7} {:<8} {:<6} {target}",
            config.enabled, config.lazy
        );
    }

    // A listing command is an explicit request, so lazy servers are contacted
    // too. One unreachable server must not hide the others.
    let registry = McpRegistry::connect_all(&ctx.config.mcp, true).await;

    let connected = registry.names();
    println!(
        "\nconnected: {}",
        if connected.is_empty() {
            "(none)".to_string()
        } else {
            connected.join(", ")
        }
    );
    for name in registry.tools().names() {
        println!("  {name}");
    }
    for failure in registry.failures() {
        eprintln!("  failed: {failure}");
    }
    Ok(())
}

async fn call(ctx: &AppContext, server: &str, tool: &str, json: &str) -> anyhow::Result<()> {
    let config = ctx.config.mcp.servers.get(server).ok_or_else(|| {
        anyhow::anyhow!("unknown MCP server `{server}`; configured: {}", known(ctx))
    })?;

    let arguments: serde_json::Value = serde_json::from_str(json)
        .map_err(|err| anyhow::anyhow!("--json is not valid JSON: {err}"))?;

    let mut handle = McpServer::connect(server, config)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    handle
        .initialize()
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    let result = handle
        .call_tool(tool, arguments)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    if let Err(err) = handle.shutdown().await {
        tracing::warn!("failed to shut the server down cleanly: {err}");
    }

    println!("{}", result.content);
    if result.is_error {
        anyhow::bail!("the server reported the call as failed");
    }
    Ok(())
}

fn known(ctx: &AppContext) -> String {
    let names: Vec<&str> = ctx.config.mcp.servers.keys().map(String::as_str).collect();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}
