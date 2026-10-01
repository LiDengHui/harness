//! A stand-in MCP server for the stdio transport tests.
//!
//! Baked into a `harness = false` target so `cargo test` builds it next to the
//! test binaries that spawn it, and so it needs no interpreter, no network and
//! no preinstalled MCP server. Cargo itself runs this binary with no arguments;
//! without `--serve` it exits immediately and counts as a passing test.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

fn main() {
    if std::env::args().any(|arg| arg == "--serve") {
        serve();
    }
}

fn serve() {
    // The tests need the child's pid to prove that shutdown really killed it.
    if let Ok(path) = std::env::var("HARNESS_MCP_FAKE_PID_FILE") {
        let _ = std::fs::write(path, std::process::id().to_string());
    }

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        // A notification carries no id and is never answered.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fake-mcp-server", "version": "0.0.1" },
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "echo",
                            "description": "Echo the given message back.",
                            "inputSchema": {
                                "type": "object",
                                "properties": { "message": { "type": "string" } },
                                "required": ["message"],
                            },
                        },
                        {
                            "name": "boom",
                            "description": "Always reports a tool-level failure.",
                            "inputSchema": { "type": "object", "properties": {} },
                        },
                    ]
                }
            }),
            "tools/call" => call(&message, &id),
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method `{other}` is not supported") },
            }),
        };

        if writeln!(stdout, "{response}").is_err() {
            break;
        }
        let _ = stdout.flush();
    }
}

fn call(message: &Value, id: &Value) -> Value {
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "echo" => {
            let text = arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": format!("echo: {text}") }],
                    "isError": false,
                }
            })
        }
        "boom" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": "tool failed on purpose" }],
                "isError": true,
            }
        }),
        other => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32602, "message": format!("unknown tool `{other}`") },
        }),
    }
}
