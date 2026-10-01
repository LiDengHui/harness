//! End-to-end tests of the stdio transport against a real child process.

mod support;

use std::time::Duration;

use harness_core::{HarnessError, McpServerConfig};
use harness_mcp::McpServer;
use serde_json::json;

use support::{fake_stdio_config, process_alive, read_pid, wait_until_gone};

#[tokio::test]
async fn a_stdio_server_is_driven_end_to_end_and_killed_on_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("server.pid");
    let mut server = McpServer::connect("fake", &fake_stdio_config(&pid_file))
        .await
        .unwrap();

    assert_eq!(server.name(), "fake");
    assert!(!server.is_connected());
    assert!(
        !pid_file.exists(),
        "building a client must not launch the server"
    );

    let info = server.initialize().await.unwrap();
    assert_eq!(info.protocol_version, "2024-11-05");
    assert_eq!(info.server_name, "fake-mcp-server");
    assert_eq!(info.server_version, "0.0.1");
    assert!(info.capabilities["tools"].is_object());
    assert!(server.is_connected());

    // The handshake is sent once; a second call replays the negotiation.
    assert_eq!(server.initialize().await.unwrap(), info);

    let tools = server.tools().await.unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["echo", "boom"]
    );
    assert_eq!(tools[0].description, "Echo the given message back.");
    assert_eq!(tools[0].input_schema["required"][0], "message");
    assert_eq!(server.tools().await.unwrap().len(), 2, "the list is cached");

    let result = server
        .call_tool("echo", json!({ "message": "hello" }))
        .await
        .unwrap();
    assert_eq!(result.content, "echo: hello");
    assert!(!result.is_error);
    assert_eq!(result.raw["isError"], false);

    let failure = server.call_tool("boom", json!({})).await.unwrap();
    assert!(failure.is_error);
    assert_eq!(failure.content, "tool failed on purpose");

    let err = server.call_tool("nope", json!({})).await.unwrap_err();
    match err {
        HarnessError::Mcp(message) => {
            assert!(message.contains("-32602"), "{message}");
            assert!(message.contains("nope"), "{message}");
        }
        other => panic!("unexpected error: {other}"),
    }

    let pid = read_pid(&pid_file);
    assert!(process_alive(pid), "the child runs until shutdown");

    server.shutdown().await.unwrap();
    assert!(!server.is_connected());
    assert!(
        wait_until_gone(pid).await,
        "shutdown must terminate the child process"
    );
}

#[tokio::test]
async fn tool_specs_are_namespaced_and_advertised_without_an_adapter() {
    let tmp = tempfile::tempdir().unwrap();
    let mut server = McpServer::connect("fake", &fake_stdio_config(&tmp.path().join("pid")))
        .await
        .unwrap();

    let specs = server.tool_specs().await.unwrap();
    assert_eq!(specs[0].name, "mcp__fake__echo");
    assert_eq!(specs[1].name, "mcp__fake__boom");
    assert_eq!(specs[0].parameters["required"][0], "message");
}

#[tokio::test]
async fn a_missing_program_fails_on_first_use_not_on_construction() {
    let config = McpServerConfig {
        command: Some("harness-mcp-no-such-program".to_string()),
        ..Default::default()
    };

    let mut server = McpServer::connect("missing", &config).await.unwrap();
    let err = server.initialize().await.unwrap_err();
    assert!(matches!(err, HarnessError::Mcp(_)), "{err}");
    assert!(
        err.to_string().contains("harness-mcp-no-such-program"),
        "{err}"
    );
    assert!(!server.is_connected());
}

#[tokio::test]
async fn a_config_with_two_transports_is_refused() {
    let config = McpServerConfig {
        command: Some("agent-mcp".to_string()),
        url: Some("http://127.0.0.1:1/mcp".to_string()),
        ..Default::default()
    };

    let err = McpServer::connect("broken", &config).await.unwrap_err();
    assert!(matches!(err, HarnessError::Config(_)), "{err}");
    assert!(err.to_string().contains("both"), "{err}");
}

#[tokio::test]
async fn a_server_that_exits_early_reports_a_closed_pipe() {
    // `cmd /c exit` on Windows and `sh -c true` elsewhere: a process that
    // starts, says nothing and is gone.
    let config = if cfg!(windows) {
        McpServerConfig {
            command: Some("cmd".to_string()),
            args: vec!["/c".to_string(), "exit".to_string()],
            ..Default::default()
        }
    } else {
        McpServerConfig {
            command: Some("sh".to_string()),
            args: vec!["-c".to_string(), "true".to_string()],
            ..Default::default()
        }
    };

    let mut server = McpServer::connect("empty", &config).await.unwrap();
    let err = tokio::time::timeout(Duration::from_secs(20), server.initialize())
        .await
        .expect("a dead server must not hang the client")
        .unwrap_err();
    assert!(matches!(err, HarnessError::Mcp(_)), "{err}");
}
