//! Adapter and registry tests: the MCP tools as the agent loop will see them.

mod support;

use std::sync::Arc;

use harness_core::{McpServerConfig, ToolsConfig};
use harness_mcp::{McpRegistry, McpServer, McpToolAdapter};
use harness_tools::{Tool, ToolContext, ToolRegistry};
use serde_json::json;
use tokio::sync::Mutex;

use support::{fake_stdio_config, mcp_config, process_alive, read_pid, wait_until_gone};

#[tokio::test]
async fn an_adapter_is_namespaced_and_dispatches_through_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let config = fake_stdio_config(&tmp.path().join("pid"));
    let server = Arc::new(Mutex::new(
        McpServer::connect("fake", &config).await.unwrap(),
    ));
    let specs = {
        let mut server = server.lock().await;
        server.tools().await.unwrap()
    };

    let adapter = McpToolAdapter::new(server.clone(), specs[0].clone());
    assert_eq!(adapter.spec().name, "mcp__fake__echo");
    assert_eq!(adapter.local_name(), "mcp__fake__echo");
    assert_eq!(adapter.remote_name(), "echo");
    assert_eq!(adapter.spec().description, "Echo the given message back.");
    assert_eq!(adapter.spec().parameters["required"][0], "message");

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(adapter));
    assert_eq!(registry.names(), vec!["mcp__fake__echo"]);

    let ctx = ToolContext::new(tmp.path(), ToolsConfig::default());
    let tool = registry.get("mcp__fake__echo").expect("registered above");
    let output = tool.call(json!({ "message": "hi" }), &ctx).await.unwrap();
    assert!(!output.is_error);
    assert_eq!(output.content, "echo: hi");
    assert_eq!(output.metadata["server"], "fake");
    assert_eq!(output.metadata["tool"], "echo");
    assert_eq!(output.metadata["is_error"], false);
    assert_eq!(output.metadata["truncated"], false);
}

#[tokio::test]
async fn a_tool_level_failure_is_an_output_error_not_a_call_error() {
    let tmp = tempfile::tempdir().unwrap();
    let config = fake_stdio_config(&tmp.path().join("pid"));
    let server = Arc::new(Mutex::new(
        McpServer::connect("fake", &config).await.unwrap(),
    ));
    let specs = {
        let mut server = server.lock().await;
        server.tools().await.unwrap()
    };

    let boom = McpToolAdapter::new(server.clone(), specs[1].clone());
    let ctx = ToolContext::new(tmp.path(), ToolsConfig::default());
    let output = boom.call(json!({}), &ctx).await.unwrap();

    assert!(output.is_error);
    assert_eq!(output.content, "tool failed on purpose");
    assert_eq!(output.metadata["is_error"], true);
}

#[tokio::test]
async fn adapters_merge_into_the_builtin_registry_without_colliding() {
    let tmp = tempfile::tempdir().unwrap();
    let config = mcp_config(&[(
        "fake",
        McpServerConfig {
            lazy: false,
            ..fake_stdio_config(&tmp.path().join("pid"))
        },
    )]);

    let mcp = McpRegistry::connect(&config).await;
    assert!(mcp.failures().is_empty(), "{:?}", mcp.failures());

    let mut registry = ToolRegistry::with_builtins();
    for name in mcp.tools().names() {
        if let Some(tool) = mcp.tools().get(&name) {
            registry.register(tool);
        }
    }

    assert!(
        registry.get("shell").is_some(),
        "builtins survive the merge"
    );
    assert!(registry.get("mcp__fake__echo").is_some());
    assert_eq!(registry.len(), 7 + 2);

    let ctx = ToolContext::new(tmp.path(), ToolsConfig::default());
    let tool = registry.get("mcp__fake__echo").unwrap();
    let output = tool
        .call(json!({ "message": "merged" }), &ctx)
        .await
        .unwrap();
    assert_eq!(output.content, "echo: merged");
}

#[tokio::test]
async fn a_registry_connects_eager_servers_and_leaves_lazy_ones_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let eager_pid = tmp.path().join("eager.pid");
    let lazy_pid = tmp.path().join("lazy.pid");

    let config = mcp_config(&[
        (
            "eager",
            McpServerConfig {
                lazy: false,
                ..fake_stdio_config(&eager_pid)
            },
        ),
        (
            "deferred",
            McpServerConfig {
                lazy: true,
                ..fake_stdio_config(&lazy_pid)
            },
        ),
    ]);

    let mut registry = McpRegistry::connect(&config).await;
    assert!(registry.failures().is_empty(), "{:?}", registry.failures());
    assert_eq!(registry.names(), vec!["eager".to_string()]);
    assert_eq!(
        registry.tools().names(),
        vec![
            "mcp__eager__boom".to_string(),
            "mcp__eager__echo".to_string()
        ]
    );
    assert!(
        !lazy_pid.exists(),
        "a lazy server must not be started at connect time"
    );

    let pid = read_pid(&eager_pid);
    assert!(process_alive(pid));

    registry.shutdown().await;
    assert!(
        wait_until_gone(pid).await,
        "shutdown must terminate the connected server"
    );
}

#[tokio::test]
async fn a_lazy_server_can_be_activated_on_demand() {
    let tmp = tempfile::tempdir().unwrap();
    let config = fake_stdio_config(&tmp.path().join("lazy.pid"));
    let config = mcp_config(&[(
        "deferred",
        McpServerConfig {
            lazy: true,
            ..config
        },
    )]);

    let mut registry = McpRegistry::connect(&config).await;
    assert!(registry.tools().is_empty());
    assert!(registry.names().is_empty());

    let names = registry
        .add("deferred", &config.servers["deferred"])
        .await
        .unwrap();
    assert_eq!(
        names,
        vec![
            "mcp__deferred__echo".to_string(),
            "mcp__deferred__boom".to_string()
        ],
        "`add` reports the server's own tool order"
    );
    assert!(registry.server("deferred").is_some());
    assert!(registry.server("unknown").is_none());
}

#[tokio::test]
async fn a_broken_server_is_recorded_without_sinking_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let config = mcp_config(&[
        (
            "broken",
            McpServerConfig {
                lazy: false,
                command: Some("harness-mcp-no-such-program".to_string()),
                ..Default::default()
            },
        ),
        (
            "good",
            McpServerConfig {
                lazy: false,
                ..fake_stdio_config(&tmp.path().join("good.pid"))
            },
        ),
    ]);

    let registry = McpRegistry::connect(&config).await;
    assert_eq!(registry.failures().len(), 1, "{:?}", registry.failures());
    assert!(
        registry.failures()[0].starts_with("broken:"),
        "{:?}",
        registry.failures()
    );
    assert_eq!(registry.names(), vec!["good".to_string()]);
    assert_eq!(registry.tools().len(), 2);

    // The adapters alone are enough to hand back to the caller.
    let tools = registry.into_tools();
    assert_eq!(tools.names()[0], "mcp__good__boom");
}
