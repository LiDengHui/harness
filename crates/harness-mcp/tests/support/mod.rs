//! Scaffolding shared by the MCP tests: the fake server binary, a loopback HTTP
//! endpoint, and process liveness checks.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use harness_core::{McpConfig, McpServerConfig};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

/// The compiled `fake_mcp_server` target, which cargo builds beside this test
/// binary. A stale build may linger, so the newest match wins.
pub fn fake_server_binary() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary has a path");
    let dir = exe.parent().expect("the test binary sits in a directory");
    let suffix = std::env::consts::EXE_SUFFIX;

    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("the deps directory is readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            name.strip_suffix(suffix)
                .is_some_and(|stem| stem.starts_with("fake_mcp_server-") && !stem.contains('.'))
        })
        .collect();

    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    });
    candidates.pop().unwrap_or_else(|| {
        panic!(
            "no fake_mcp_server binary in {}; run `cargo test -p harness-mcp` so cargo builds \
             every test target of this package",
            dir.display()
        )
    })
}

/// A stdio config that launches the fake server, telling it where to record its
/// pid so a test can prove the process was really killed.
pub fn fake_stdio_config(pid_file: &Path) -> McpServerConfig {
    let mut env = BTreeMap::new();
    env.insert(
        "HARNESS_MCP_FAKE_PID_FILE".to_string(),
        pid_file.to_string_lossy().into_owned(),
    );
    McpServerConfig {
        command: Some(fake_server_binary().to_string_lossy().into_owned()),
        args: vec!["--serve".to_string()],
        env,
        ..Default::default()
    }
}

pub fn mcp_config(servers: &[(&str, McpServerConfig)]) -> McpConfig {
    let mut config = McpConfig::default();
    for (name, server) in servers {
        config.servers.insert((*name).to_string(), server.clone());
    }
    config
}

/// The pid the fake server wrote on startup.
pub fn read_pid(path: &Path) -> u32 {
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the fake server never wrote its pid to {}", path.display())
}

pub fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let filter = format!("PID eq {pid}");
        match std::process::Command::new("tasklist")
            .args(["/FI", &filter, "/NH", "/FO", "CSV"])
            .output()
        {
            // CSV rows are quoted, so this cannot match a longer pid.
            Ok(output) => String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")),
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

/// Waits for the process to disappear, since a kill is not instantaneous.
pub async fn wait_until_gone(pid: u32) -> bool {
    for _ in 0..100 {
        if !process_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !process_alive(pid)
}

/// How the loopback endpoint frames its answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    Json,
    Sse,
}

pub struct TestHttpServer {
    pub url: String,
    /// Connections accepted so far, so a test can prove nothing was dialled.
    pub connections: Arc<AtomicUsize>,
    /// Each request body, in arrival order.
    pub requests: UnboundedReceiver<Value>,
}

pub fn url_config(server: &TestHttpServer) -> McpServerConfig {
    McpServerConfig {
        url: Some(server.url.clone()),
        ..Default::default()
    }
}

/// A loopback HTTP/1.1 endpoint that answers MCP requests.
///
/// Keep-alive is intentional: the client is supposed to reuse its connection,
/// and a test that dialled per request would go unnoticed otherwise.
pub async fn serve_mcp(framing: Framing) -> TestHttpServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available");
    let addr = listener.local_addr().expect("the listener has an address");
    let (sender, requests) = unbounded_channel();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = connections.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let sender = sender.clone();
            tokio::spawn(async move {
                loop {
                    let Some(body) = read_request(&mut socket).await else {
                        return;
                    };
                    let Ok(request) = serde_json::from_str::<Value>(&body) else {
                        return;
                    };
                    if sender.send(request.clone()).is_err() {
                        return;
                    }
                    if socket
                        .write_all(render(framing, &request).as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    TestHttpServer {
        url: format!("http://{addr}/mcp"),
        connections,
        requests,
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut content_length = 0usize;
    for line in head.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    while buffer.len() < header_end + content_length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    Some(String::from_utf8_lossy(&buffer[header_end..]).to_string())
}

fn render(framing: Framing, request: &Value) -> String {
    // Notifications are acknowledged with an empty body.
    let Some(id) = request.get("id").cloned() else {
        return "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".to_string();
    };

    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let response = match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "http-fake", "version": "0.0.2" },
            }
        }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [{
                    "name": "echo",
                    "description": "Echo the given message back.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "message": { "type": "string" } },
                        "required": ["message"],
                    },
                }]
            }
        }),
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or_default();
            if name == "echo" {
                let text = request["params"]["arguments"]["message"]
                    .as_str()
                    .unwrap_or_default();
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": format!("http: {text}") }],
                        "isError": false,
                    }
                })
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": format!("unknown tool `{name}`") },
                })
            }
        }
        other => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method `{other}` is not supported") },
        }),
    };

    match framing {
        Framing::Json => {
            let body = response.to_string();
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        }
        Framing::Sse => {
            // Decoys come first so that matching on `id`, rather than on
            // "the first frame", is what the test actually exercises.
            let body = format!(
                concat!(
                    "event: message\n",
                    "data: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{{}}}}\n",
                    "\n",
                    "data: {{\"jsonrpc\":\"2.0\",\"id\":999999,\"result\":{{\"decoy\":true}}}}\n",
                    "\n",
                    "data: {response}\n",
                    "\n",
                ),
                response = response,
            );
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        }
    }
}
