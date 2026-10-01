//! The JSON-RPC 2.0 envelopes and the MCP payloads that travel in them.

use serde_json::{json, Value};

use harness_core::{HarnessError, Result, ToolSpec};

/// The JSON-RPC revision every message carries.
pub(crate) const JSONRPC_VERSION: &str = "2.0";

/// The protocol revision this client opens with. A server may negotiate lower
/// by answering with a different `protocolVersion`.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

pub(crate) const CLIENT_NAME: &str = "harness";

/// Namespace that keeps a remote tool from shadowing a builtin.
pub const TOOL_PREFIX: &str = "mcp";

/// The local registration name of a remote tool, `mcp__<server>__<tool>`.
pub fn qualified_name(server: &str, tool: &str) -> String {
    format!("{TOOL_PREFIX}__{server}__{tool}")
}

pub(crate) fn request(id: u64, method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "method": method,
        "params": params,
    })
}

pub(crate) fn notification(method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "method": method,
        "params": params,
    })
}

/// The request a message answers, or `None` when it is not a response.
pub(crate) fn response_id(message: &Value) -> Option<u64> {
    message.get("id").and_then(Value::as_u64)
}

/// Unwraps a response's `result`, turning an `error` member into
/// [`HarnessError::Mcp`] that still carries the server's own code and message.
pub(crate) fn take_result(message: &Value) -> Result<Value> {
    if let Some(error) = message.get("error") {
        return Err(server_error(error));
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

pub(crate) fn server_error(error: &Value) -> HarnessError {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no message");
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => HarnessError::Mcp(format!("server error {code}: {message}")),
        None => HarnessError::Mcp(format!("server error: {message}")),
    }
}

/// What a server reported during the `initialize` handshake.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerInfo {
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
    pub capabilities: Value,
}

impl McpServerInfo {
    pub(crate) fn from_value(result: &Value) -> Self {
        let server = result.get("serverInfo");
        Self {
            protocol_version: result
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION)
                .to_string(),
            server_name: text_of(server, "name"),
            server_version: text_of(server, "version"),
            capabilities: result
                .get("capabilities")
                .cloned()
                .unwrap_or_else(|| json!({})),
        }
    }
}

fn text_of(value: Option<&Value>, key: &str) -> String {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One tool exactly as the server advertises it.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolSpec {
    /// The name the server knows this tool by.
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl McpToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }

    /// The form to advertise to a model, named so it cannot collide with a
    /// builtin: a remote `shell` and the local `shell` must stay distinct.
    pub fn to_tool_spec(&self, server: &str) -> ToolSpec {
        ToolSpec::new(
            qualified_name(server, &self.name),
            self.description.clone(),
            self.input_schema.clone(),
        )
    }

    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        let name = value.get("name").and_then(Value::as_str)?;
        Some(Self {
            name: name.to_string(),
            description: value
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input_schema: value
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object" })),
        })
    }

    /// Reads a `tools/list` result. A malformed entry is dropped rather than
    /// failing the whole server: one bad tool should not hide the good ones.
    pub(crate) fn list_from(result: &Value) -> Result<Vec<Self>> {
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                HarnessError::Mcp("tools/list response carries no `tools` array".to_string())
            })?;
        Ok(tools.iter().filter_map(Self::from_value).collect())
    }
}

/// The outcome of a `tools/call`.
#[derive(Debug, Clone, PartialEq)]
pub struct McpCallResult {
    /// The text of the result's content blocks, newline-joined.
    pub content: String,
    /// The server's own tool-level failure flag. A transport or protocol
    /// failure is an `Err` instead; this is "the tool ran and reported failure".
    pub is_error: bool,
    pub raw: Value,
}

impl McpCallResult {
    pub(crate) fn from_value(result: Value) -> Self {
        let content = match result.get("content").and_then(Value::as_array) {
            Some(blocks) => blocks.iter().map(block_text).collect::<Vec<_>>().join("\n"),
            // No content blocks at all: hand over the raw result rather than
            // an empty string, which would look like a successful no-op.
            None => result.to_string(),
        };
        Self {
            content,
            is_error: result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            raw: result,
        }
    }
}

fn block_text(block: &Value) -> String {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        // Images and embedded resources have no textual form; name the type so
        // the model knows something arrived instead of seeing nothing.
        Some(other) => format!("[{other} content]"),
        None => block.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_tool_gets_a_namespaced_name() {
        assert_eq!(qualified_name("files", "read"), "mcp__files__read");
        let spec = McpToolSpec::new("read", "reads", json!({ "type": "object" }));
        let advertised = spec.to_tool_spec("files");
        assert_eq!(advertised.name, "mcp__files__read");
        assert_eq!(advertised.description, "reads");
    }

    #[test]
    fn a_server_error_keeps_the_code_and_the_message() {
        let err = take_result(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32602, "message": "unknown tool `nope`" },
        }))
        .unwrap_err();

        match err {
            HarnessError::Mcp(message) => {
                assert!(message.contains("-32602"), "{message}");
                assert!(message.contains("unknown tool `nope`"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }

        let code_less = server_error(&json!({ "message": "boom" }));
        assert!(code_less.to_string().contains("boom"), "{code_less}");
    }

    #[test]
    fn a_result_without_an_error_member_is_unwrapped() {
        let value =
            take_result(&json!({ "jsonrpc": "2.0", "id": 7, "result": { "ok": true } })).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(response_id(&json!({ "id": 7 })), Some(7));
        assert_eq!(response_id(&json!({ "method": "notifications/x" })), None);
    }

    #[test]
    fn the_handshake_result_is_parsed_and_defaulted() {
        let info = McpServerInfo::from_value(&json!({
            "protocolVersion": "2025-03-26",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "example", "version": "2.1.0" },
        }));
        assert_eq!(info.protocol_version, "2025-03-26");
        assert_eq!(info.server_name, "example");
        assert_eq!(info.server_version, "2.1.0");
        assert!(info.capabilities["tools"].is_object());

        // A bare result still yields something usable.
        let bare = McpServerInfo::from_value(&json!({}));
        assert_eq!(bare.protocol_version, PROTOCOL_VERSION);
        assert_eq!(bare.server_name, "");
    }

    #[test]
    fn the_tool_list_drops_entries_without_a_name() {
        let tools = McpToolSpec::list_from(&json!({
            "tools": [
                { "name": "echo", "description": "echoes", "inputSchema": { "type": "object", "required": ["message"] } },
                { "description": "anonymous" },
                { "name": "bare" },
            ]
        }))
        .unwrap();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].input_schema["required"][0], "message");
        assert_eq!(tools[1].name, "bare");
        assert_eq!(tools[1].description, "");
        assert_eq!(tools[1].input_schema["type"], "object");

        let err = McpToolSpec::list_from(&json!({})).unwrap_err();
        assert!(err.to_string().contains("tools"), "{err}");
    }

    #[test]
    fn call_results_join_text_blocks_and_name_the_rest() {
        let result = McpCallResult::from_value(json!({
            "content": [
                { "type": "text", "text": "first" },
                { "type": "image", "data": "aGk=", "mimeType": "image/png" },
                { "type": "text", "text": "second" },
            ],
            "isError": true,
        }));

        assert_eq!(result.content, "first\n[image content]\nsecond");
        assert!(result.is_error);
        assert_eq!(result.raw["isError"], true);

        // A content-less result falls back to its own JSON.
        let bare = McpCallResult::from_value(json!({ "structured": 1 }));
        assert!(!bare.is_error);
        assert!(bare.content.contains("structured"), "{}", bare.content);
    }
}
