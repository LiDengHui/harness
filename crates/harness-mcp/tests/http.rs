//! End-to-end tests of the streamable HTTP transport over a loopback socket.

mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use harness_core::HarnessError;
use harness_mcp::McpServer;
use serde_json::json;

use support::{serve_mcp, url_config, Framing, TestHttpServer};

async fn methods_seen(server: &mut TestHttpServer) -> Vec<String> {
    let mut seen = Vec::new();
    while let Ok(request) = server.requests.try_recv() {
        seen.push(
            request
                .get("method")
                .and_then(|method| method.as_str())
                .unwrap_or_default()
                .to_string(),
        );
    }
    seen
}

#[tokio::test]
async fn a_plain_json_endpoint_answers_every_request() {
    let mut server = serve_mcp(Framing::Json).await;
    let mut client = McpServer::connect("remote", &url_config(&server))
        .await
        .unwrap();

    let info = client.initialize().await.unwrap();
    assert_eq!(info.protocol_version, "2024-11-05");
    assert_eq!(info.server_name, "http-fake");
    assert_eq!(info.server_version, "0.0.2");
    assert!(client.is_connected());

    let tools = client.tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].input_schema["required"][0], "message");

    let result = client
        .call_tool("echo", json!({ "message": "hi" }))
        .await
        .unwrap();
    assert_eq!(result.content, "http: hi");
    assert!(!result.is_error);

    let err = client.call_tool("missing", json!({})).await.unwrap_err();
    match err {
        HarnessError::Mcp(message) => {
            assert!(message.contains("-32602"), "{message}");
            assert!(message.contains("missing"), "{message}");
        }
        other => panic!("unexpected error: {other}"),
    }

    client.shutdown().await.unwrap();
    assert!(!client.is_connected());

    assert_eq!(
        methods_seen(&mut server).await,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call",
            "tools/call",
        ]
    );
    assert_eq!(
        server.connections.load(Ordering::SeqCst),
        1,
        "one client per server means one reused connection"
    );
}

#[tokio::test]
async fn an_sse_endpoint_is_read_frame_by_frame() {
    let server = serve_mcp(Framing::Sse).await;
    let mut client = McpServer::connect("remote", &url_config(&server))
        .await
        .unwrap();

    // Every answer here is an event stream, decoys and all, so this covers the
    // handshake, the tool list and a call.
    let info = client.initialize().await.unwrap();
    assert_eq!(info.server_name, "http-fake");

    let tools = client.tools().await.unwrap();
    assert_eq!(tools[0].name, "echo");

    let result = client
        .call_tool("echo", json!({ "message": "streamed" }))
        .await
        .unwrap();
    assert_eq!(result.content, "http: streamed");

    let err = client.call_tool("missing", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("-32602"), "{err}");
}

#[tokio::test]
async fn building_a_client_touches_the_network_zero_times() {
    let server = serve_mcp(Framing::Json).await;
    let connections = server.connections.clone();

    let mut client = McpServer::connect("remote", &url_config(&server))
        .await
        .unwrap();
    assert!(!client.is_connected());

    // Deferred schemas are the point: a configured server that is never used
    // must not cost a socket, a handshake or a tool list.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "constructing a client must not dial the server"
    );

    client.tools().await.unwrap();
    assert!(connections.load(Ordering::SeqCst) >= 1);
    assert!(client.is_connected());
}

#[tokio::test]
async fn an_unreachable_server_reports_the_transport_failure() {
    // Bound and dropped, so the port is almost certainly free.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let config = harness_core::McpServerConfig {
        url: Some(format!("http://{addr}/mcp")),
        ..Default::default()
    };

    let mut client = McpServer::connect("gone", &config).await.unwrap();
    let err = client.initialize().await.unwrap_err();
    assert!(matches!(err, HarnessError::Mcp(_)), "{err}");
    assert!(err.to_string().contains(&addr.to_string()), "{err}");
}
