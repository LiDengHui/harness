//! Remote MCP tools presented as local ones.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use harness_core::{Result, ToolSpec};
use harness_tools::{Tool, ToolContext, ToolOutput};

use crate::protocol::{qualified_name, McpToolSpec};
use crate::server::McpServer;

/// One remote tool, registered as `mcp__<server>__<tool>`.
pub struct McpToolAdapter {
    server: Arc<Mutex<McpServer>>,
    spec: McpToolSpec,
    local_name: String,
}

impl McpToolAdapter {
    /// Wraps `spec` as a tool of `server`.
    ///
    /// The qualified name is resolved here because [`Tool::spec`] is synchronous
    /// and cannot await the server mutex. Build adapters while the server is
    /// idle — that is, from the registry, which is the only supported path.
    pub fn new(server: Arc<Mutex<McpServer>>, spec: McpToolSpec) -> Self {
        let local_name = match server.try_lock() {
            Ok(server) => qualified_name(server.name(), &spec.name),
            // Only reachable if a caller builds an adapter while the server is
            // mid-call. The tool still works; it just loses its namespace.
            Err(_) => spec.name.clone(),
        };
        Self {
            server,
            spec,
            local_name,
        }
    }

    /// The name this tool is registered under locally.
    pub fn local_name(&self) -> &str {
        &self.local_name
    }

    /// The name the remote server knows this tool by.
    pub fn remote_name(&self) -> &str {
        &self.spec.name
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            self.local_name.clone(),
            self.spec.description.clone(),
            self.spec.input_schema.clone(),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let mut server = self.server.lock().await;
        let name = server.name().to_string();
        // The handshake — and, when the caller never listed them, the schema
        // fetch — is paid here, on the first call that needs it.
        let result = server.call_tool(&self.spec.name, args).await?;

        // A remote tool can answer with megabytes of text, so its output enters
        // the context under the same ceiling the builtins respect — and, like
        // them, it says so in its content rather than only in its metadata. The
        // agent loop forwards only `content` to the model, so a fact kept in
        // `metadata` is a fact the model never sees.
        let total_bytes = result.content.len();
        let (mut content, truncated) =
            truncate_on_char_boundary(&result.content, ctx.config.max_output_bytes);
        if truncated {
            content.push_str(&truncation_marker(content.len(), total_bytes));
        }

        let metadata = json!({
            "server": name,
            "tool": self.spec.name,
            "is_error": result.is_error,
            "bytes": content.len(),
            "total_bytes": total_bytes,
            "truncated": truncated,
        });
        let output = if result.is_error {
            ToolOutput::error(content)
        } else {
            ToolOutput::text(content)
        };
        Ok(output.with_metadata(metadata))
    }
}

/// Truncates on a character boundary and reports whether anything was dropped.
fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// The marker appended when a remote tool's output was cut.
///
/// Mirrored from `harness_tools::builtin::truncation_marker` rather than
/// imported: that helper is `pub(crate)` to `harness-tools`, so it is not
/// reachable from this crate, and widening another crate's API for one format
/// string is not worth it. The two must stay in step — the builtins and the MCP
/// adapter feed the same model, so one marker wording is what keeps a truncated
/// answer recognisable wherever it came from.
fn truncation_marker(shown: usize, total: usize) -> String {
    let dropped = total.saturating_sub(shown);
    format!(
        "\n\n[output truncated: showed {shown} of {total} bytes; {dropped} bytes dropped. \
         Raise `tools.max_output_bytes` in the config, or call the remote tool with a \
         narrower request.]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_never_splits_a_character() {
        let (kept, truncated) = truncate_on_char_boundary("héllo", 100);
        assert_eq!(kept, "héllo");
        assert!(!truncated);

        // The second byte of `é` is off limits, so the cut happens before it.
        let (kept, truncated) = truncate_on_char_boundary("héllo", 2);
        assert_eq!(kept, "h");
        assert!(truncated);
    }

    #[test]
    fn the_truncation_marker_names_what_was_shown_and_dropped() {
        let marker = truncation_marker(10, 25);
        assert!(marker.contains("showed 10 of 25 bytes"), "{marker}");
        assert!(marker.contains("15 bytes dropped"), "{marker}");
        assert!(marker.contains("max_output_bytes"), "{marker}");
    }
}
